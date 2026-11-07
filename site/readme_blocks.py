"""Generated blocks of README.md: `<!-- NAME:start -->` ... `<!-- NAME:end -->`.

usage:
  python3 site/readme_blocks.py set <readme> <name> <content-file>
      replace the content of block NAME with the file's text;
  python3 site/readme_blocks.py copy <from-readme> <to-readme> <name>...
      copy blocks from one README to another (the Pages workflow uses it to
      carry the regenerated benchmark blocks of a release over to master).
A block missing from either file is reported and skipped.
"""

import os
import sys


def markers(name):
    return f"<!-- {name}:start -->", f"<!-- {name}:end -->"


def get(text, name):
    start, end = markers(name)
    a, b = text.find(start), text.find(end)
    if a < 0 or b < a:
        return None
    return text[a + len(start):b]


def put(text, name, content):
    start, end = markers(name)
    a, b = text.find(start), text.find(end)
    if a < 0 or b < a:
        return None
    return text[:a + len(start)] + content + text[b:]


def write(path, text):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(text)
    os.replace(tmp, path)


def main(argv):
    if len(argv) == 4 and argv[0] == "set":
        readme, name, content_file = argv[1:]
        with open(readme, encoding="utf-8") as f:
            text = f.read()
        with open(content_file, encoding="utf-8") as f:
            content = "\n" + f.read().strip("\n") + "\n"
        new = put(text, name, content)
        if new is None:
            print(f"readme_blocks: {readme} has no {name} block, skipped")
            return 0
        if new != text:
            write(readme, new)
            print(f"readme_blocks: {name} updated in {readme}")
        return 0
    if len(argv) >= 4 and argv[0] == "copy":
        source, target, names = argv[1], argv[2], argv[3:]
        with open(source, encoding="utf-8") as f:
            src = f.read()
        with open(target, encoding="utf-8") as f:
            text = f.read()
        new = text
        for name in names:
            content = get(src, name)
            replaced = None if content is None else put(new, name, content)
            if replaced is None:
                print(f"readme_blocks: block {name} missing in {source} or {target}, skipped")
                continue
            new = replaced
        if new != text:
            write(target, new)
            print(f"readme_blocks: {', '.join(names)} copied into {target}")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
