#!/bin/sh
# Writes a cache file under $HOME the way Python tooling does, then prints
# $HOME so the test can check the whole directory went away afterwards.
mkdir -p "$HOME/.cache/pip" && echo x > "$HOME/.cache/pip/entry"
printf '%s' "$HOME"
