"""A beater.js Python tool: module-level TOOL metadata + a run() entrypoint.

Runs in the CPython interpreter embedded in the beater host process —
the full ML ecosystem (numpy, torch, pandas) is importable here.
"""

TOOL = {
    "description": "Summarize a list of numbers: count, sum, mean, min, max.",
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
