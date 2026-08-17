#!/usr/bin/env python3
"""Reports the published runs of one zkVM as the tables a comparison is pasted from.

Usage: report.py --dir <published profiles> --zkvm <name>

The index names every run published under the directory, so the newest run of each guest is picked
out of it and only the profiles behind those are read. Two runs are comparable only when the harness,
the zkVM version and the corpus all agree, since costs from two SDK versions come from different
circuits and two corpora share no block at all, so the newest run of the zkVM fixes those three and a
guest contributes its own newest run that matches them. Guests are then compared over the blocks all
of them profiled, so one that failed on some does not come out cheaper for having skipped them.
"""

import argparse
import json
import os

# Digits of the ELF hash a published profile is filed under.
ELF_SHA256_PREFIX = 16


def read(path):
    """Parses a JSON file, naming the file in whatever goes wrong with it."""
    try:
        with open(path) as handle:
            return json.load(handle)
    except (OSError, ValueError) as error:
        raise SystemExit(f"failed to read {path}, {error}")


def published(directory, zkvm):
    """The newest run of each guest of `zkvm`, alongside the run they are all comparable with."""
    index = read(os.path.join(directory, "index.json"))
    # Ordered here rather than taken from the index, so the newest is the newest by the field that
    # says so.
    listed = sorted(
        (run for run in index["runs"] if run["zkvm"] == zkvm),
        key=lambda run: run["generated_at"],
        reverse=True,
    )
    if not listed:
        raise SystemExit(f"the index lists no run on {zkvm}")
    newest = listed[0]
    comparable = {}
    for run in listed:
        if all(run[field] == newest[field] for field in ("version", "zkvm_version", "suite")):
            comparable.setdefault(run["stateless_validator"], run)
    return newest, [comparable[guest] for guest in sorted(comparable)]


def profile(directory, run):
    """The profile a run published, which sits at the path its own fields compose."""
    sha256 = run["elf_sha256"]
    if not sha256:
        raise SystemExit(f"the run of {run['stateless_validator']} records no ELF hash")
    return read(
        os.path.join(
            directory,
            run["stateless_validator"],
            sha256[:ELF_SHA256_PREFIX],
            run["suite"] + ".json",
        )
    )


def series(run, document, shared, kinds):
    """One guest's figures over the shared blocks, in the units it was profiled in."""
    label = run["stateless_validator"]
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
        "version": run["stateless_validator_version"],
        "sha256": short_sha256(run["elf_sha256"]),
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
    parser.add_argument(
        "--dir", default="profiles", help="directory the profiles are published under"
    )
    parser.add_argument("--zkvm", required=True, help="zkVM to report the published runs of")
    arguments = parser.parse_args()

    newest, runs = published(arguments.dir, arguments.zkvm)
    documents = [profile(arguments.dir, run) for run in runs]
    shared = set.intersection(*(set(document["profile"]) for document in documents))
    if not shared:
        raise SystemExit(f"the published runs on {arguments.zkvm} share no profiled block")
    kinds = newest["composition"]
    guests = [
        series(run, document, shared, [kind["name"] for kind in kinds])
        for run, document in zip(runs, documents)
    ]
    print(report(newest, guests, len(shared), kinds))


if __name__ == "__main__":
    main()
