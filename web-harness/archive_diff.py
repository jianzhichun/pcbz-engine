"""Summarize how a saved project archive differs from the archive it was opened from.

Usage: python3 archive_diff.py <before.pc> <after.pc>

Prints JSON: entry counts, the added, removed and changed member names, and each changed
member's before/after sizes. Member content is compared by SHA-256.
"""

import hashlib
import json
import sys
import zipfile


def entries(path):
    with zipfile.ZipFile(path) as archive:
        return {info.filename: (info.file_size, hashlib.sha256(archive.read(info)).hexdigest()) for info in archive.infolist()}


def main():
    before, after = entries(sys.argv[1]), entries(sys.argv[2])
    added = sorted(set(after) - set(before))
    removed = sorted(set(before) - set(after))
    changed = sorted(name for name in set(before) & set(after) if before[name][1] != after[name][1])
    print(
        json.dumps(
            {
                "before_entries": len(before),
                "after_entries": len(after),
                "added": added,
                "removed": removed,
                "changed": changed,
                "changed_detail": {name: {"before_bytes": before[name][0], "after_bytes": after[name][0]} for name in changed},
            },
            indent=2,
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    main()