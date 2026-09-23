#!/bin/sh
# Stands in for Microsoft's markitdown in pidge-mcp's tests: prints a marker
# line, then the file it was given. A file whose extension is `.fail` makes
# it fail the way a real conversion error does (stderr noise, exit 3), so
# tests can check that stderr never reaches the caller. A `.empty` file
# converts to nothing, like a scanned PDF with no text layer.
case "$1" in
  *.empty)
    exit 0
    ;;
  *.fail)
    echo "Traceback: secret-stderr-detail" >&2
    exit 3
    ;;
esac
echo "# converted"
cat "$1"
