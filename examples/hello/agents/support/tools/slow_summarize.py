"""Crash/resume fixture: a slow idempotent summarization tool."""

import time


TOOL = {
    "description": (
        "Crash/resume test fixture. Use only when the user explicitly asks "
        "for slow_summarize by name. Waits 15 seconds before returning and "
        "is safe to retry after a crash."
    ),
    "input_schema": {
        "type": "object",
        "properties": {
            "numbers": {
                "type": "array",
                "items": {"type": "number"},
                "description": "The numbers to summarize.",
            }
        },
        "required": ["numbers"],
    },
}


def run(input):
    time.sleep(15)
    nums = [float(n) for n in input["numbers"]]
    if not nums:
        return {"count": 0}
    return {
        "count": len(nums),
        "sum": sum(nums),
        "mean": sum(nums) / len(nums),
        "min": min(nums),
        "max": max(nums),
    }
