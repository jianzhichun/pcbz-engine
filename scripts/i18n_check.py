"""Check the Chinese interface catalog and count user-visible English literals left in the
chrome/app crates' own Rust sources.

Usage (inside the public build tree): python i18n_check.py <repo root> [--max-english N]
The tree root is the checkout that holds `upstream/` (the patched tangency-labs/pcbcraft copy).

Checks, all fatal except the count:
  - `crates/chrome/assets/shared-ui/zh.json` parses and translates every `en.json` key
    (placeholders preserved);
  - no catalog string names the upstream products, the SolidCraft brand, or a person;
  - prints the number of still-English user-visible literals drawn directly by egui in
    `crates/chrome/src` and `crates/app/src` (labels the shared kit draws are translated by
    `solidcraft_ui_text::tr_label`, so only egui-native text is counted). `--max-english` fails
    above the given budget; omitted, the count is reported only (the first cut's honest number).
Stdlib only.
"""
import json
import pathlib
import re
import sys

# Person names are written as code points so the repository scan never carries them literally.
FORBIDDEN = ("PCBCraft", "PCBcraft", "pcbcraft", "SolidCraft", "\u5f20", "\u6602", "\u007a\u007a\u0063\u0068\u0075\u006e")
# egui-native sinks that draw the literal themselves. Kit helpers (controls::button, menu rows,
# shell::dialog, settings::, workspace::, ...) resolve labels through solidcraft_ui_text::tr_label.
DIRECT = (
    "ui.label(", "ui.heading(", "ui.small(", "ui.button(", "ui.selectable_label(", "ui.checkbox(",
    "ui.radio_value(", "ui.add(", "RichText::new(", "egui::Button::new(", "Button::new(", "Label::new(",
    "egui::Window::new(", "Window::new(", "menu_button(", "on_hover_text(", "on_disabled_hover_text(",
)
STR = re.compile(r'"((?:[^"\\]|\\.)*)"')


def literal_after(line, start):
    match = STR.search(line, start)
    return match.group(1) if match else None


def user_visible(text):
    if not text or "{" in text or "\\" in text:
        return False
    if not re.search("[A-Za-z]", text):
        return False
    if re.fullmatch(r"[a-z0-9_.\-/:]+", text):
        return False
    if re.fullmatch(r"[A-Za-z0-9_]+", text):
        return False
    return True


def main():
    args = [a for a in sys.argv[1:]]
    max_english = None
    if "--max-english" in args:
        i = args.index("--max-english")
        max_english = int(args[i + 1])
        del args[i:i + 2]
    root = pathlib.Path(args[0] if args else ".").resolve()
    chrome = root / "upstream" / "crates" / "chrome"
    en = json.loads((chrome / "assets/shared-ui/en.json").read_text(encoding="utf-8"))
    zh = json.loads((chrome / "assets/shared-ui/zh.json").read_text(encoding="utf-8"))

    problems = []
    missing = [k for k in en if k not in zh]
    if missing:
        problems.append(f"zh.json is missing {len(missing)} en keys: {missing[:10]}")
    for key, value in zh.items():
        low = value.lower()
        for word in FORBIDDEN:
            if word.lower() in low:
                problems.append(f"zh.json[{key!r}] names {word!r}: {value!r}")
        if re.search(r"\{[^}]*\}", en.get(key, "")) and sorted(re.findall(r"\{[^}]*\}", en.get(key, ""))) != sorted(re.findall(r"\{[^}]*\}", value)):
            problems.append(f"zh.json[{key!r}] changed the placeholders of {en.get(key)!r}")

    remaining = []
    for crate in ("chrome", "app"):
        for path in sorted((root / "upstream" / "crates" / crate / "src").rglob("*.rs")):
            try:
                lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
            except OSError:
                continue
            for number, line in enumerate(lines, 1):
                if "matches!" in line:
                    continue  # a literal compared, not drawn
                for sink in DIRECT:
                    start = line.find(sink)
                    while start >= 0:
                        start += len(sink)
                        match = STR.search(line, start)
                        before = line[max(0, match.start() - 60):match.start()] if match is not None else ""
                        kit = any(k in before for k in ("controls::", "ui_kit::", "solidcraft_ui_kit::", "settings::", "workspace::", "menu::", "board_setup::"))
                        if match is not None and user_visible(match.group(1)) and "tr_label(" not in before and not kit:
                            remaining.append(f"{path.relative_to(root)}:{number}: {match.group(1)}")
                        start = line.find(sink, start)

    for problem in problems:
        print(f"i18n: {problem}")
    print(f"i18n: zh catalog keys={len(zh)} en keys={len(en)} missing={len(missing)} forbidden={sum(1 for p in problems if 'names' in p)}")
    print(f"i18n: remaining English UI literals in chrome/app (direct egui sinks)={len(remaining)}")
    for line in remaining[:40]:
        print(f"  {line}")
    if len(remaining) > 40:
        print(f"  ... {len(remaining) - 40} more")

    if problems:
        return 1
    if max_english is not None and len(remaining) > max_english:
        print(f"i18n: {len(remaining)} English literals exceed the budget {max_english}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())