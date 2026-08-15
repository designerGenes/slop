use std::cmp::min;
use std::io::{self, Stdout, Write};

use crossterm::cursor::{MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

use crate::error::SlopError;

const STATUS: &str = " Manual deslop mode | Ctrl-D to deslop | Ctrl-C to cancel ";

pub fn read_document() -> Result<String, SlopError> {
    terminal::enable_raw_mode().map_err(SlopError::TerminalInteractionFailure)?;
    let mut output = io::stdout();
    if let Err(error) = execute!(output, EnterAlternateScreen, Show, EnableBracketedPaste) {
        let _ = terminal::disable_raw_mode();
        return Err(SlopError::TerminalInteractionFailure(error));
    }

    let result = edit(&mut output);
    let cleanup = restore_terminal(&mut output);
    match (result, cleanup) {
        (Ok(document), Ok(())) => Ok(document),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(SlopError::TerminalInteractionFailure(error)),
    }
}

fn edit(output: &mut Stdout) -> Result<String, SlopError> {
    let mut buffer = EditorBuffer::default();
    render(output, &buffer)?;

    loop {
        match event::read().map_err(SlopError::TerminalInteractionFailure)? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match apply_key(&mut buffer, key) {
                    EditorAction::Continue => render(output, &buffer)?,
                    EditorAction::Submit => return Ok(buffer.into_string()),
                    EditorAction::Cancel => return Err(SlopError::ManualDeslopCancelled),
                }
            }
            Event::Paste(text) => {
                buffer.insert_text(&text.replace("\r\n", "\n").replace('\r', "\n"));
                render(output, &buffer)?;
            }
            Event::Resize(_, _) => render(output, &buffer)?,
            _ => {}
        }
    }
}

fn restore_terminal(output: &mut Stdout) -> io::Result<()> {
    let screen_result = execute!(output, DisableBracketedPaste, Show, LeaveAlternateScreen);
    let raw_result = terminal::disable_raw_mode();
    screen_result.and(raw_result)
}

fn render(output: &mut Stdout, buffer: &EditorBuffer) -> Result<(), SlopError> {
    let (width, height) = terminal::size().map_err(SlopError::TerminalInteractionFailure)?;
    let content_height = usize::from(height.saturating_sub(1)).max(1);
    let (cursor_line, cursor_column) = buffer.cursor_position();
    let first_line = cursor_line.saturating_sub(content_height.saturating_sub(1));
    let first_column = cursor_column.saturating_sub(usize::from(width).saturating_sub(1));

    queue!(output, MoveTo(0, 0), Clear(ClearType::All))
        .map_err(SlopError::TerminalInteractionFailure)?;
    for (screen_line, line) in buffer
        .lines()
        .iter()
        .skip(first_line)
        .take(content_height)
        .enumerate()
    {
        let visible = line
            .chars()
            .skip(first_column)
            .take(usize::from(width))
            .collect::<String>();
        queue!(output, MoveTo(0, screen_line as u16), Print(visible))
            .map_err(SlopError::TerminalInteractionFailure)?;
    }

    let status_row = height.saturating_sub(1);
    let status = STATUS.chars().take(usize::from(width)).collect::<String>();
    queue!(
        output,
        MoveTo(0, status_row),
        SetAttribute(Attribute::Reverse),
        Print(status),
        SetAttribute(Attribute::Reset),
        Show,
        MoveTo(
            min(
                cursor_column.saturating_sub(first_column),
                usize::from(width).saturating_sub(1)
            ) as u16,
            min(
                cursor_line.saturating_sub(first_line),
                content_height.saturating_sub(1)
            ) as u16
        )
    )
    .map_err(SlopError::TerminalInteractionFailure)?;
    output
        .flush()
        .map_err(SlopError::TerminalInteractionFailure)
}

#[derive(Default)]
struct EditorBuffer {
    text: Vec<char>,
    cursor: usize,
}

impl EditorBuffer {
    fn into_string(self) -> String {
        self.text.into_iter().collect()
    }

    fn insert_text(&mut self, text: &str) {
        let inserted = text.chars().collect::<Vec<_>>();
        self.cursor += inserted.len();
        self.text.splice(
            self.cursor - inserted.len()..self.cursor - inserted.len(),
            inserted,
        );
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    fn line_start(&self, index: usize) -> usize {
        self.text[..index]
            .iter()
            .rposition(|character| *character == '\n')
            .map_or(0, |newline| newline + 1)
    }

    fn line_end(&self, index: usize) -> usize {
        self.text[index..]
            .iter()
            .position(|character| *character == '\n')
            .map_or(self.text.len(), |newline| index + newline)
    }

    fn cursor_position(&self) -> (usize, usize) {
        let line = self.text[..self.cursor]
            .iter()
            .filter(|character| **character == '\n')
            .count();
        (line, self.cursor - self.line_start(self.cursor))
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.cursor = min(self.cursor + 1, self.text.len());
    }

    fn move_up(&mut self) {
        let start = self.line_start(self.cursor);
        if start == 0 {
            return;
        }
        let column = self.cursor - start;
        let previous_end = start - 1;
        let previous_start = self.line_start(previous_end);
        self.cursor = previous_start + min(column, previous_end - previous_start);
    }

    fn move_down(&mut self) {
        let end = self.line_end(self.cursor);
        if end == self.text.len() {
            return;
        }
        let column = self.cursor - self.line_start(self.cursor);
        let next_start = end + 1;
        let next_end = self.line_end(next_start);
        self.cursor = next_start + min(column, next_end - next_start);
    }

    fn move_home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    fn move_end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    fn lines(&self) -> Vec<String> {
        self.text
            .iter()
            .collect::<String>()
            .split('\n')
            .map(ToString::to_string)
            .collect()
    }
}

enum EditorAction {
    Continue,
    Submit,
    Cancel,
}

fn apply_key(buffer: &mut EditorBuffer, key: KeyEvent) -> EditorAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('d') => EditorAction::Submit,
            KeyCode::Char('c') => EditorAction::Cancel,
            _ => EditorAction::Continue,
        };
    }

    match key.code {
        KeyCode::Char(character) => buffer.insert_text(&character.to_string()),
        KeyCode::Enter => buffer.insert_text("\n"),
        KeyCode::Backspace => buffer.backspace(),
        KeyCode::Delete => buffer.delete(),
        KeyCode::Left => buffer.move_left(),
        KeyCode::Right => buffer.move_right(),
        KeyCode::Up => buffer.move_up(),
        KeyCode::Down => buffer.move_down(),
        KeyCode::Home => buffer.move_home(),
        KeyCode::End => buffer.move_end(),
        _ => {}
    }
    EditorAction::Continue
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{apply_key, EditorAction, EditorBuffer};

    #[test]
    fn arrow_keys_move_within_and_between_lines() {
        let mut buffer = EditorBuffer::default();
        buffer.insert_text("abc\ndef\nxyz");
        buffer.cursor = 2;

        apply_key(
            &mut buffer,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        assert_eq!(buffer.cursor, 6);
        apply_key(
            &mut buffer,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        );
        assert_eq!(buffer.cursor, 5);
        apply_key(&mut buffer, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(buffer.cursor, 1);
    }

    #[test]
    fn ctrl_d_submits_and_ctrl_c_cancels() {
        let mut buffer = EditorBuffer::default();
        assert!(matches!(
            apply_key(
                &mut buffer,
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)
            ),
            EditorAction::Submit
        ));
        assert!(matches!(
            apply_key(
                &mut buffer,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            EditorAction::Cancel
        ));
    }
}
