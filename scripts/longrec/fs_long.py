"""Freesound search for LONG geotagged recordings (>= 15 min) uploaded since 2023-03.

Same as scripts/testset/fs_search.py, with a duration filter.
usage: python3 -I fs_long.py OUTDIR 'query' ... > hits.jsonl
"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "testset"))
import fs_search  # noqa: E402

fs_search.FILTER = ("is_geotagged:1 duration:[900 TO *] created:[2023-03-01T00:00:00Z TO NOW] "
                    + os.environ.get("FS_FILTER_EXTRA", "")).strip()  # e.g. FS_FILTER_EXTRA=username:klankbeeld

if __name__ == "__main__":
    fs_search.main()
