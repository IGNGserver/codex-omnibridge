#!/usr/bin/env bash
# Regenerate the panel's self-hosted fonts.
#
# The panel ships subsetted variable fonts instead of loading them from
# fonts.googleapis.com: the desktop app loads the page over file://, where a
# CDN request fails offline, and self-hosting lets the CSP keep font-src 'self'
# with no third-party origin.
#
# This script is a one-time asset-generation step. Its outputs are committed:
#   apps/panel/fonts/roboto-flex-latin.woff2        (Roboto Flex, Latin)
#   apps/panel/fonts/roboto-flex-latin-ext.woff2    (Roboto Flex, Latin Ext)
#   apps/panel/fonts/noto-sans-sc.woff2             (Noto Sans SC, CJK)
#   apps/panel/fonts/material-symbols-outlined.woff2
#
# Run it when the icon set changes, or when the CJK character coverage needs to
# change. After adding an icon to ICONS below, re-run and also add the matching
# `.m3-i-*` class to apps/panel/icons.css.
#
# Requires: network access, python3 with fonttools+brotli (pip install fonttools brotli).
#
#   ./scripts/build-panel-fonts.sh
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FONT_DIR="$ROOT/apps/panel/fonts"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

UA="Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"

# CJK coverage: GB2312 level 1 (the 3755 most common hanzi) plus the panel's own
# strings, ASCII, CJK punctuation and fullwidth forms.
#
# Level 1 rather than level 2 or the full set is a deliberate size/coverage
# trade. Measured on Noto Sans SC:
#   panel strings only (438 hanzi)  142 KB  -> mixed-font rendering for any
#                                              user-typed name, worse than none
#   GB2312 level 1 (3755 hanzi)    1013 KB  -> covers everyday Chinese text
#   GB2312 full    (6763 hanzi)    1807 KB
# User-supplied text (account nicknames, provider names, model names) is what
# makes the narrow option unusable: a subset that only knows the panel's own
# wording renders "Plus 主号" in two different typefaces.
CJK_SOURCE_URL="https://raw.githubusercontent.com/google/fonts/main/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf"

# Every icon the panel renders. Keep in sync with the .m3-i-* classes.
ICONS=(
  account_circle add add_task arrow_drop_down autorenew badge bookmark_add
  brightness_auto check check_circle dark_mode dashboard delete desktop_windows
  dns edit error help hub info install_desktop light_mode login logout
  manage_accounts neurology playlist_add refresh restart_alt router save
  security settings_backup_restore swap_horiz sync travel_explore tune
  upload_file verified_user visibility visibility_off
)

PYTHON="${PYTHON:-python3}"
if ! "$PYTHON" -c "import fontTools, brotli" 2>/dev/null; then
  echo "error: fonttools and brotli are required (pip install fonttools brotli)" >&2
  exit 1
fi

mkdir -p "$FONT_DIR"

# ---------------------------------------------------------------- Roboto Flex
echo "==> fetching Roboto Flex css"
curl -sS -m 30 -H "User-Agent: $UA" \
  "https://fonts.googleapis.com/css2?family=Roboto+Flex:opsz,wght@8..144,100..1000&display=swap" \
  -o "$TMP/rf.css"

"$PYTHON" - "$TMP/rf.css" "$TMP" <<'PY'
import re, sys, pathlib
css = pathlib.Path(sys.argv[1]).read_text()
out = pathlib.Path(sys.argv[2])
# Split the stylesheet into /* subset */ @font-face blocks and keep url + range.
blocks = re.findall(r"/\*\s*([\w-]+)\s*\*/\s*@font-face\s*\{(.*?)\}", css, re.S)
kept = 0
for name, body in blocks:
    if name not in ("latin", "latin-ext"):
        continue
    url = re.search(r"url\((https://[^)]+\.woff2)\)", body).group(1)
    (out / f"rf-{name}.url").write_text(url)
    kept += 1
print(f"  found {kept} latin subsets")
PY

for subset in latin latin-ext; do
  echo "==> downloading Roboto Flex ($subset)"
  curl -sS -m 60 -H "User-Agent: $UA" "$(cat "$TMP/rf-$subset.url")" -o "$TMP/rf-$subset.woff2"
  cp "$TMP/rf-$subset.woff2" "$FONT_DIR/roboto-flex-$subset.woff2"
done

# ------------------------------------------------------------------- Noto Sans SC
# Roboto Flex carries no CJK, so without this the Chinese UI would fall back to
# whatever each machine happens to have installed -- which is exactly what the
# panel did before. Subsetting the variable font keeps the wght axis, so the
# emphasized type roles (500/700) still apply to Chinese text.
echo "==> fetching Noto Sans SC"
curl -sS -L -m 300 -H "User-Agent: $UA" "$CJK_SOURCE_URL" -o "$TMP/noto-sans-sc-full.ttf"
echo "  full font: $(( $(stat -c%s "$TMP/noto-sans-sc-full.ttf") / 1024 / 1024 )) MB"

echo "==> building CJK character set"
"$PYTHON" - "$ROOT" "$TMP" <<'PY'
import pathlib, sys

root, tmp = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])

chars = set()

# 1. Everything the panel's own source can display.
for rel in ("apps/panel/index.html", "apps/panel/main.js"):
    for ch in (root / rel).read_text(encoding="utf-8"):
        chars.add(ch)

# 2. ASCII, CJK punctuation and fullwidth forms (always needed).
for cp in (
    list(range(0x20, 0x7F))
    + list(range(0xA0, 0x100))
    + list(range(0x2000, 0x2070))
    + list(range(0x20A0, 0x20C0))
    + list(range(0x3000, 0x3040))
    + list(range(0xFF00, 0xFF66))
):
    chars.add(chr(cp))

# 3. GB2312 level 1: the 3755 most common simplified hanzi. This is what makes
#    user-typed names (account nicknames, provider and model names) render in
#    one typeface instead of switching family mid-string.
for hi in range(0xB0, 0xD8):
    for lo in range(0xA1, 0xFF):
        try:
            chars.add(bytes([hi, lo]).decode("gb2312"))
        except UnicodeDecodeError:
            pass

text = "".join(sorted(c for c in chars if c.strip()))
(tmp / "cjk-chars.txt").write_text(text, encoding="utf-8")
print(f"  {len(text)} characters")
PY

"$PYTHON" -m fontTools.subset "$TMP/noto-sans-sc-full.ttf" \
  --output-file="$FONT_DIR/noto-sans-sc.woff2" \
  --flavor=woff2 \
  --text-file="$TMP/cjk-chars.txt" \
  --layout-features='kern,liga,locl,ccmp,mark,mkmk' \
  --no-hinting --desubroutinize
echo "  subset: $(( $(stat -c%s "$FONT_DIR/noto-sans-sc.woff2") / 1024 )) KB"

# ----------------------------------------------------------- Material Symbols
echo "==> fetching Material Symbols"
curl -sS -m 30 -H "User-Agent: $UA" \
  "https://fonts.googleapis.com/css2?family=Material+Symbols+Outlined:opsz,wght,FILL,GRAD@20..48,100..700,0..1,-50..200" \
  -o "$TMP/ms.css"
MS_URL="$(grep -o 'https://[^)]*\.woff2' "$TMP/ms.css" | tail -1)"
curl -sS -m 120 -H "User-Agent: $UA" "$MS_URL" -o "$TMP/ms-full.woff2"
echo "  full font: $(stat -c%s "$TMP/ms-full.woff2") bytes"

# Resolve each icon name to its codepoint. Names are addressed by codepoint in
# the markup because a ligature name silently stops working when upstream
# renames an icon (file_upload and restore both moved during this rewrite).
echo "==> resolving icon codepoints"
"$PYTHON" - "$TMP/ms-full.woff2" "$TMP" "${ICONS[@]}" <<'PY'
import json, sys, pathlib
from fontTools.ttLib import TTFont

font = TTFont(sys.argv[1])
out = pathlib.Path(sys.argv[2])
wanted = sys.argv[3:]

gsub = font["GSUB"].table
lig_glyphs = {}
for lookup in gsub.LookupList.Lookup:
    for st in lookup.SubTable:
        inner = st.ExtSubTable if hasattr(st, "ExtSubTable") else st
        if getattr(inner, "ligatures", None):
            for _first, lst in inner.ligatures.items():
                for lig in lst:
                    lig_glyphs[lig.LigGlyph] = "".join(lig.Component)

cmap = font.getBestCmap()
glyph_to_cp = {}
for cp, gname in cmap.items():
    glyph_to_cp.setdefault(gname, cp)

resolved, missing = {}, []
for name in wanted:
    cp = glyph_to_cp.get(name) or glyph_to_cp.get(lig_glyphs.get(name, ""))
    if cp:
        resolved[name] = cp
    else:
        missing.append(name)

if missing:
    raise SystemExit(f"error: no codepoint for: {', '.join(missing)}")

json.dump(resolved, open(out / "icons.json", "w"), indent=1, sort_keys=True)
# Emit the codepoint list for the subsetter.
(out / "unicodes.txt").write_text(",".join(f"U+{cp:04X}" for cp in sorted(set(resolved.values()))))
print(f"  resolved {len(resolved)} icons")
PY

"$PYTHON" -m fontTools.subset "$TMP/ms-full.woff2" \
  --output-file="$FONT_DIR/material-symbols-outlined.woff2" \
  --flavor=woff2 \
  --unicodes="$(cat "$TMP/unicodes.txt")" \
  --layout-features='' --no-hinting --desubroutinize --drop-tables+=GSUB

echo "==> regenerating apps/panel/icons.css"
"$PYTHON" - "$TMP/icons.json" "$ROOT/apps/panel/icons.css" <<'PY'
import json, sys, pathlib
icons = json.load(open(sys.argv[1]))
header = '''/* ==========================================================================
   Self-hosted icon font + glyph classes.
   GENERATED by scripts/build-panel-fonts.sh -- edit that script, not this file.

   The panel previously pulled Material Symbols from fonts.googleapis.com, which
   meant icons vanished in the offline Electron window and forced the CSP to
   allow a third-party origin. This is the same variable font subset to the
   glyphs the panel actually uses, so the FILL/wght/GRAD/opsz axes still work
   and no network is required.

   Icons are addressed by codepoint class rather than by ligature name: a
   codepoint survives subsetting unconditionally, whereas a ligature silently
   stops working the moment upstream renames an icon.
   ========================================================================== */

@font-face {
  font-family: 'Material Symbols Outlined';
  font-style: normal;
  font-weight: 100 700;
  font-display: block;
  src: url('fonts/material-symbols-outlined.woff2') format('woff2-variations');
}

/* Roboto Flex: self-hosted for the same reason. The two subsets cover Latin and
   Latin Extended; CJK falls through to the system stack in tokens.css. */
@font-face {
  font-family: 'Roboto Flex';
  font-style: normal;
  font-weight: 100 1000;
  font-display: swap;
  src: url('fonts/roboto-flex-latin.woff2') format('woff2-variations');
  unicode-range: U+0000-00FF, U+0131, U+0152-0153, U+02BB-02BC, U+02C6, U+02DA,
    U+02DC, U+0304, U+0308, U+0329, U+2000-206F, U+2074, U+20AC, U+2122, U+2191,
    U+2193, U+2212, U+2215, U+FEFF, U+FFFD;
}

@font-face {
  font-family: 'Roboto Flex';
  font-style: normal;
  font-weight: 100 1000;
  font-display: swap;
  src: url('fonts/roboto-flex-latin-ext.woff2') format('woff2-variations');
  unicode-range: U+0100-02BA, U+02BD-02C5, U+02C7-02CC, U+02CE-02D7, U+02DD-02FF,
    U+0304, U+0308, U+0329, U+1D00-1DBF, U+1E00-1E9F, U+1EF2-1EFF, U+2020,
    U+20A0-20AB, U+20AD-20C0, U+2113, U+2C60-2C7F, U+A720-A7FF;
}

/* Noto Sans SC carries the CJK glyphs Roboto Flex does not have. It is declared
   after Roboto Flex and scoped to the CJK ranges, so Latin inside a Chinese
   string still renders in Roboto Flex -- the two stack rather than compete.
   The wght axis survives subsetting, so emphasized roles apply here too.
   GENERATED from: scripts/build-panel-fonts.sh */
@font-face {
  font-family: 'Noto Sans SC';
  font-style: normal;
  font-weight: 100 900;
  font-display: swap;
  src: url('fonts/noto-sans-sc.woff2') format('woff2-variations');
  unicode-range: U+2E80-2EFF, U+2F00-2FDF, U+3000-303F, U+3040-30FF, U+3100-312F,
    U+31A0-31BF, U+31F0-31FF, U+3200-32FF, U+3300-33FF, U+3400-4DBF, U+4E00-9FFF,
    U+F900-FAFF, U+FE10-FE1F, U+FE30-FE4F, U+FF00-FFEF, U+20000-2A6DF;
}

/* Glyph classes: one per icon the panel uses. */
'''
lines = [header]
for name, cp in sorted(icons.items()):
    lines.append(f'.m3-i-{name.replace("_", "-")}::before {{ content: "\\{cp:04x}"; }}')
lines.append("")
pathlib.Path(sys.argv[2]).write_text("\n".join(lines), encoding="utf-8")
print(f"  wrote {len(icons)} icon classes")
PY

echo "==> done"
ls -la "$FONT_DIR"
