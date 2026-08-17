#!/usr/bin/env python3
"""Reports the runs of one zkVM under a directory as the tables a comparison is pasted from.

Usage: report.py --dir <profiles> --zkvm <name>

Every profile under the directory is read and stated by the meta it carries, which holds every field
an index lists a run by, so a published tree and the artifacts of one workflow run report alike. Two
runs are comparable only when the harness, the zkVM version and the corpus all agree, since costs
from two SDK versions come from different circuits and two corpora share no block at all, so the
newest run of the zkVM fixes those three and a guest contributes its own newest run that matches
them. Guests are then compared over the blocks all of them profiled, so one that failed on some does
not come out cheaper for having skipped them.
"""

import argparse
import json
import os


def read(path):
    """Parses a JSON file, naming the file in whatever goes wrong with it."""
    try:
        with open(path) as handle:
            return json.load(handle)
    except (OSError, ValueError) as error:
        raise SystemExit(f"failed to read {path}, {error}")


def profiles(directory):
    """Every profile under the directory, which is every JSON file in it but the index itself.

    The index is excluded by the path it sits at rather than by the name it carries, so a corpus
    called index keeps its profile read.
    """
    if not os.path.isdir(directory):
        raise SystemExit(f"{directory} is no directory of profiles")
    index = os.path.join(directory, "index.json")
    found = []
    for walked, directories, filenames in os.walk(directory):
        directories.sort()
        for filename in sorted(filenames):
            path = os.path.join(walked, filename)
            if filename.endswith(".json") and path != index:
                found.append(read(path))
    return found


def comparable(directory, zkvm):
    """The newest run of each guest of `zkvm`, alongside the run they are all comparable with."""
    listed = sorted(
        (document for document in profiles(directory) if document["meta"]["zkvm"] == zkvm),
        key=lambda document: document["meta"]["generated_at"],
        reverse=True,
    )
    if not listed:
        raise SystemExit(f"{directory} holds no profile of {zkvm}")
    newest = listed[0]["meta"]
    picked = {}
    for document in listed:
        meta = document["meta"]
        if all(meta[field] == newest[field] for field in ("version", "zkvm_version", "suite")):
            picked.setdefault(meta["stateless_validator"], document)
    return newest, [picked[guest] for guest in sorted(picked)]


def series(document, shared, kinds):
    """One guest's figures over the shared blocks, in the units it was profiled in."""
    meta = document["meta"]
    label = meta["stateless_validator"]
    if not meta["elf_sha256"]:
        raise SystemExit(f"the run of {label} records no ELF hash")
    blocks = [document["profile"][name] for name in shared]
    for name in sorted(shared):
        cost = document["profile"][name]["cost"]
        missing = [kind for kind in kinds if kind not in cost]
        if missing:
            raise SystemExit(f"{label}/{name} has no {' or '.join(missing)} cost")
        # A stacked bar that does not reach its own total would misstate every share it draws. Costs
        # are whole numbers of cells, so the parts either add up or they do not.
        summed = sum(cost[kind] for kind in kinds)
        if kinds and summed != cost["total"]:
            raise SystemExit(
                f"{label}/{name} costs {summed} across its kinds, not the total of {cost['total']}"
            )
    peaks = [block["peak_heap_bytes"] for block in blocks if "peak_heap_bytes" in block]
    return {
        "label": label,
        "version": meta["stateless_validator_version"],
        "sha256": short_sha256(meta["elf_sha256"]),
        "total": sum(block["cost"]["total"] for block in blocks),
        "components": [sum(block["cost"][kind] for block in blocks) for kind in kinds],
        # A mean rather than the largest, since one figure per guest is there to compare how much
        # memory the corpus takes them rather than to size a machine for their worst block.
        "peak": sum(peaks) / len(peaks) if peaks else None,
    }


def si(value):
    """Formats a magnitude with an SI suffix, which keeps costs past 10^10 readable."""
    for limit, suffix in ((1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "k")):
        if abs(value) >= limit:
            return f"{value / limit:.2f}{suffix}"
    return f"{value:.0f}"


def byte_size(value):
    """Formats a byte count in binary units, which is how a memory figure reads and how the page
    prints the same number."""
    for limit, unit in ((1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")):
        if abs(value) >= limit:
            return f"{value / limit:.2f} {unit}"
    return f"{value:.0f} B"


def short_sha256(sha256):
    """The ELF's hash as the page shows it, the first and last four digits of the hex."""
    return f"0x{sha256[:4]}...{sha256[-4:]}"


def report(newest, guests, blocks, kinds):
    """The same figures as the page, as the tables a report is pasted from.

    Cost is stated per block, dividing the total a guest carries by the block count so it reads
    beside the mean peak heap rather than against a different span. Ratios are taken between the
    totals and between the means, since an average of ratios reads differently depending on which
    guest it is stated against.
    """
    cheapest = min(guest["total"] for guest in guests)
    lines = [
        "# zkEVM guest cost estimation",
        "",
        f"Profiles of {len(guests)} guests over {blocks} blocks of `{newest['suite']}`,"
        f" on {newest['zkvm']} {newest['zkvm_version']}.",
        "",
        "## Cost",
        "",
        "| Guest | Version | Avg cost | Relative | ELF SHA256 |",
        "| --- | --- | ---: | ---: | ---: |",
    ]
    for guest in guests:
        lines.append(
            f"| {guest['label']} | {guest['version']} | {si(guest['total'] / blocks)}"
            f" | {guest['total'] / cheapest:.2f}x | {guest['sha256']} |"
        )

    if kinds:
        lines += [
            "",
            "## Cost composition",
            "",
            "| Guest |" + "".join(f" {kind['name']} |" for kind in kinds),
            "| --- |" + " ---: |" * len(kinds),
        ]
        for guest in guests:
            cells = "".join(
                f" {si(value / blocks)} ({value / guest['total'] * 100:.1f}%) |"
                for value in guest["components"]
            )
            lines.append(f"| {guest['label']} |{cells}")
        lines.append("")
        lines += [f"- `{kind['name']}`: {kind['note']}" for kind in kinds]

    heap = [guest for guest in guests if guest["peak"] is not None]
    if heap:
        smallest = min(guest["peak"] for guest in heap)
        lines += [
            "",
            "## Peak heap",
            "",
            "Mean of the peaks a guest reached over the corpus.",
            "",
            "| Guest | Version | Avg peak heap | Relative |",
            "| --- | --- | ---: | ---: |",
        ]
        for guest in heap:
            lines.append(
                f"| {guest['label']} | {guest['version']} | {byte_size(guest['peak'])}"
                f" | {guest['peak'] / smallest:.2f}x |"
            )
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", default="profiles", help="directory the profiles sit under")
    parser.add_argument("--zkvm", required=True, help="zkVM to report the runs of")
    arguments = parser.parse_args()

    newest, documents = comparable(arguments.dir, arguments.zkvm)
    shared = set.intersection(*(set(document["profile"]) for document in documents))
    if not shared:
        raise SystemExit(f"the runs on {arguments.zkvm} share no profiled block")
    kinds = newest["composition"]
    guests = [series(document, shared, [kind["name"] for kind in kinds]) for document in documents]
    print(report(newest, guests, len(shared), kinds))


if __name__ == "__main__":
    main()
