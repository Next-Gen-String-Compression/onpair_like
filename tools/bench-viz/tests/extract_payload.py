#!/usr/bin/env python3
"""Pull the embedded payload out of a built explorer, for render.test.js.

    python3 tools/bench-viz/tests/extract_payload.py out/index.html payload.json

Testing the renderer against a real run rather than a fixture is the point: a
fixture only contains the cases you thought of, and the failures worth catching
are the ones you did not -- a column with no cover facts, a query with zero
admitted rows, a series measured in one run and not another.
"""

import json
import re
import sys
from pathlib import Path


def tag(html: str, name: str):
    match = re.search(
        rf'<script id="bench-viz-{name}" type="application/json">(.*?)</script>',
        html, re.S)
    if match is None:
        raise SystemExit(f"no bench-viz-{name} block in that file")
    # json_for_script escapes "<" so a config string cannot close the tag.
    return json.loads(match.group(1).replace("\\u003c", "<"))


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    html = Path(sys.argv[1]).read_text(encoding="utf-8")
    payload = {"data": tag(html, "data"), "analysis": tag(html, "analysis")}
    Path(sys.argv[2]).write_text(json.dumps(payload), encoding="utf-8")
    print(f"{len(payload['data'])} rows, "
          f"{len(payload['analysis'].get('models') or {})} fitted series")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
