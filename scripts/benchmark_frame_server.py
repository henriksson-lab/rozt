#!/usr/bin/env python3
"""Measure a running newvolim frame server without trusting a successful HTTP status alone.

The server's ``Server-Timing: render;dur=…`` value covers its blocking Palace/PNG interval.
This client reports that separately from wall-clock request duration, which also includes queue
wait, loopback/network transfer, and HTTP body read. It validates every response as a PNG before
including it in summary statistics.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import statistics
import time
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
SERVER_TIMING_RENDER = re.compile(r"(?:^|,)\s*render\s*;\s*dur=([0-9]+(?:\.[0-9]+)?)")


@dataclass(frozen=True)
class Sample:
    end_to_end_ms: float
    render_ms: float
    png_bytes: int


def percentile(samples: list[float], value: float) -> float:
    """Nearest-rank percentile, defined even for one requested frame."""
    ordered = sorted(samples)
    index = max(0, min(len(ordered) - 1, math.ceil(len(ordered) * value) - 1))
    return ordered[index]


def frame_sample(url: str, payload: dict[str, object], timeout_seconds: float) -> Sample:
    request = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
            body = response.read()
            timing = response.headers.get("Server-Timing", "")
            content_type = response.headers.get_content_type()
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"frame request returned HTTP {error.code}: {body}") from error
    elapsed_ms = (time.perf_counter() - started) * 1_000.0
    match = SERVER_TIMING_RENDER.search(timing)
    if content_type != "image/png" or not body.startswith(PNG_SIGNATURE):
        raise RuntimeError(
            f"frame response is not a PNG (content type {content_type!r}, {len(body)} bytes)"
        )
    if match is None:
        raise RuntimeError(f"frame response has no parseable render Server-Timing value: {timing!r}")
    return Sample(
        end_to_end_ms=elapsed_ms,
        render_ms=float(match.group(1)),
        png_bytes=len(body),
    )


def summary(samples: list[Sample]) -> dict[str, object]:
    def stats(values: list[float]) -> dict[str, float]:
        return {
            "min": min(values),
            "median": statistics.median(values),
            "p95": percentile(values, 0.95),
            "max": max(values),
        }

    return {
        "requests": len(samples),
        "endToEndMs": stats([sample.end_to_end_ms for sample in samples]),
        "renderMs": stats([sample.render_ms for sample in samples]),
        "pngBytes": sorted({sample.png_bytes for sample in samples}),
        "samples": [asdict(sample) for sample in samples],
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:9876/v1/frame")
    parser.add_argument("--dataset", required=True, help="configured server dataset name")
    parser.add_argument("--width", type=int, default=192)
    parser.add_argument("--height", type=int, default=144)
    parser.add_argument("--requests", type=int, default=3)
    parser.add_argument("--timeout-seconds", type=float, default=60.0)
    parser.add_argument("--orbit-x", type=int, default=0)
    parser.add_argument("--orbit-y", type=int, default=0)
    parser.add_argument("--zoom", type=float, default=1.0)
    arguments = parser.parse_args()
    if arguments.width <= 0 or arguments.height <= 0:
        parser.error("--width and --height must be positive")
    if arguments.requests <= 0:
        parser.error("--requests must be positive")
    if arguments.timeout_seconds <= 0:
        parser.error("--timeout-seconds must be positive")
    return arguments


def main() -> int:
    arguments = parse_args()
    payload = {
        "dataset": arguments.dataset,
        "width": arguments.width,
        "height": arguments.height,
        "orbitX": arguments.orbit_x,
        "orbitY": arguments.orbit_y,
        "zoom": arguments.zoom,
    }
    samples = [
        frame_sample(arguments.url, payload, arguments.timeout_seconds)
        for _ in range(arguments.requests)
    ]
    print(json.dumps(summary(samples), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
