# Custom actions and plugins

Open the main menu → **Custom Actions & Plugins…**.

## Custom actions

Add commands that appear under **More → Actions** in the context menu for matching files:

- **Command** – an argument list written with shell-style quoting (`code --goto "%f"`). It is **not** run through a shell, so odd file names cannot inject commands.
- **Applies to** – `any`, `files`, `folders`, patterns such as `*.png`, or MIME types such as `image/*` (comma separated). Every selected item must match.
- **Wait for it to finish** – shows a progress row and reports the result; otherwise the program is simply started.

Placeholders: `%f` first path · `%F` all paths (as separate arguments) · `%d` current folder · `%n` file name · `%b` name without extension · `%u` URI · `%%` a percent sign.

## Plugins

A plugin is a folder containing `plugin.toml`, placed in `~/.local/share/luma/plugins/` (or `/usr/share/luma/plugins/`).
**New plugins start switched off** – read what they run, then enable them in the manager. Plugins are declarative: nothing executes when one is loaded.

```toml
id = "image-tools"            # letters, digits, - _ .
name = "Image Tools"
version = "1.0"
description = "Small helpers for pictures"

[[actions]]                   # context-menu actions
id = "half"
name = "Resize to 50%"
command = ["convert", "%f", "-resize", "50%", "%d/%b-small.png"]
mime = ["image/*"]            # and/or extensions = ["png"], folders = true, files = true
wait = true                   # show progress and report the result

[[metadata]]                  # extra rows in Properties → Details
title = "ImageMagick"
command = ["identify", "-verbose", "%f"]     # prints "Key: Value" (or key=value) lines
mime = ["image/*"]
```

Limits: manifests up to 64 KB, 50 actions per plugin, metadata commands time out after 10 seconds. Plugins that fail to load are listed with the reason.
