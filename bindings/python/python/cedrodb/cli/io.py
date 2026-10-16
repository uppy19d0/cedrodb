"""Bounded, strict JSON input shared by document and search commands."""
import argparse
import json
import math
import sys
from pathlib import Path

MAX_INPUT_BYTES = 16 * 1024 * 1024


def bounded_integer(low, high):
    def parse(value):
        number = int(value)
        if not low <= number <= high:
            raise argparse.ArgumentTypeError("must be in {}..{}".format(low, high))
        return number
    return parse


def _reject_constant(value):
    raise ValueError("non-finite JSON number: " + value)


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key: " + key)
        result[key] = value
    return result


def _finite_json_float(text):
    value = float(text)
    if not math.isfinite(value):
        raise ValueError("non-finite JSON number: " + text)
    return value


def strict_json(text):
    if len(text.encode("utf-8")) > MAX_INPUT_BYTES:
        raise ValueError("JSON input exceeds 16 MiB")
    return json.loads(text, parse_constant=_reject_constant, parse_float=_finite_json_float, object_pairs_hook=_object)


def json_argument(text):
    try:
        return strict_json(text)
    except (ValueError, RecursionError) as error:
        raise argparse.ArgumentTypeError(str(error)) from error


def input_json(args):
    if args.value is not None:
        return args.value["json"]
    if args.file is not None:
        with Path(args.file).open("rb") as source:
            raw = source.read(MAX_INPUT_BYTES + 1)
    else:
        raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        raise ValueError("JSON input exceeds 16 MiB")
    return strict_json(raw.decode("utf-8"))


def add_input(parser):
    group = parser.add_mutually_exclusive_group(required=True)
    # Keep literal null distinct from an option that was not supplied.
    group.add_argument("--value", type=lambda text: {"json": json_argument(text)}, help="inline JSON; prefer --stdin for private data")
    group.add_argument("--file", type=Path, help="read one JSON document from a UTF-8 file")
    group.add_argument("--stdin", action="store_true", help="read one JSON document from standard input")


def finite_float(value):
    number = float(value)
    if not math.isfinite(number):
        raise argparse.ArgumentTypeError("must be finite")
    return number
