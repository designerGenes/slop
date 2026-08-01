# SlopIgnore Syntax for VS Code

This local extension registers `.slopignore` as the `slopignore` language. It assigns the `keyword.control.slopinclude` scope to directives beginning with `slopinclude` or `+`.

The workspace setting in `/Users/jadennation/DEV/01_active_projects/slop/.vscode/settings.json` colors that scope yellow (`#FFD700`) only while a `.slopignore` file is active.

## Install Locally

From `/Users/jadennation/DEV/01_active_projects/slop/editors/vscode/slopignore-syntax`, run:

```bash
pnpm dlx @vscode/vsce package
code --install-extension slopignore-syntax-0.1.0.vsix --force
```

Reload VS Code after installation. The extension has no runtime code or dependencies.
