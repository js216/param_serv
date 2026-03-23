#!/bin/sh
{
    echo '#set page(width: 6.2in, height: 8.3in, margin: (bottom:0.7in, rest:0.4in), numbering: "1/1")'
    echo '#set text(font: "Liberation Mono", size: 12pt)'
    echo '#show raw.where(block: true): it => {'
    echo '  let lines = it.text.split("\n")'
    echo '  let w = str(lines.len()).len()'
    echo '  grid(columns: (auto, 1fr), column-gutter: 0.6em, row-gutter: 0.4em,'
    echo '    ..lines.enumerate().map(((i, l)) => ('
    echo '      align(right, text(fill: luma(170), str(i+1))),'
    echo '      raw(l, lang: it.lang),'
    echo '    )).flatten())'
    echo '}'

    for f in $(find . -type f | sort); do
        ext="${f##*.}"
        case "$ext" in
            rs)   lang="rust" ;;
            html) lang="html" ;;
            toml) lang="toml" ;;
            *)    continue ;;
        esac
        echo "= $f"
        echo "#raw(read(\"/$f\"), lang: \"$lang\", block: true)"
    done
} | typst compile --root . - target/param_serv.pdf
