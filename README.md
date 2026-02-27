# `pdftui`

A terminal-based PDF viewer with SyncTeX support for Neovim.

Designed to be performant, very responsive, and work well with even very large PDFs. Built with [`ratatui`](https://github.com/ratatui-org/ratatui).

![What it looks like](./images/screenshot.png)

## Features

- SyncTeX support (forward/inverse search with Neovim)
- Asynchronous rendering
- Searching
- Hot reloading
- Responsive details about rendering/search progress
- Reactive layout

## Installation

**Option 1 — Pre-built binaries (no Rust required)**

Download the latest release for your platform from [Releases](https://github.com/tofunori/pdftui/releases):

| Platform | File |
|----------|------|
| macOS Apple Silicon | `pdftui-macos-arm64.tar.gz` |
| macOS Intel | `pdftui-macos-x86_64.tar.gz` |
| Linux x86_64 | `pdftui-linux-x86_64.tar.gz` |

```bash
tar xzf pdftui-macos-arm64.tar.gz
mv pdftui pdftui-sync ~/.local/bin/
```

**Option 2 — Build from source**

Install the [Rust toolchain](https://rustup.rs), then:

```bash
cargo install --git https://github.com/tofunori/pdftui.git --bin pdftui --bin pdftui-sync
```

On Linux, also install `libfontconfig1-dev` and `clang` first.

To use with `epub` or `cbz` files, add `--features epub`, `--features cbz`, or `--features cbz,epub`.

## Neovim integration

Add to your lazy.nvim config:

```lua
{
  "tofunori/pdftui",
  config = function()
    require("pdftui").setup({
      pdf_path = nil,  -- auto-detected from .tex file
      split = false,   -- false = open in new tab, true = split
    })
  end,
}
```

**Commands:**

| Command | Description |
|---------|-------------|
| `:PdftuiOpen [file.pdf]` | Open PDF in a new tab |
| `:PdftuiSplit [file.pdf]` | Open PDF in a split |
| `:PdftuiForward` | Jump to current cursor position in PDF |

**Inverse search:** `Ctrl+click` on the PDF jumps to the corresponding line in Neovim.

## To Build

```bash
git clone https://github.com/tofunori/pdftui.git
cd pdftui
cargo build --release
```

Binaries will be at `./target/release/pdftui` and `./target/release/pdftui-sync`.

## Can I contribute?

Yeah, sure. Please do. All contributions will be treated as licensed under MPL-2.0.
