# SlopIgnore Syntax for VS Code

This local extension registers `.slopignore` as the `slopignore` language. It assigns the `keyword.control.slopinclude` scope to directives beginning with `slopinclude` or `+`, and `keyword.control.slopheap` to complete opening and closing slopheap directives.

The extension contributes a language-specific default that colors both scopes yellow (`#FFD700`) whenever a `.slopignore` file is active, including when the file is opened outside the slop project workspace. The project setting in `/Users/jadennation/DEV/01_active_projects/slop/.vscode/settings.json` mirrors that default for the source workspace.

## Install Locally

From `/Users/jadennation/DEV/01_active_projects/slop/editors/vscode/slopignore-syntax`, run:

```bash
pnpm dlx @vscode/vsce package
code --install-extension slopignore-syntax-0.1.1.vsix --force
```

Reload VS Code after installation. The extension has no runtime code or dependencies.
