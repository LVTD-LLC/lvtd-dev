"""Report LLVM branch outcomes, refusing missing/empty branch instrumentation."""
import json
import pathlib
import sys


def summarize(report):
    if report.get("type") != "llvm.coverage.json.export":
        raise ValueError("Expected an LLVM coverage export")
    covered = total = 0
    rows = []
    for unit in report["data"]:
        branches = unit["totals"]["branches"]
        count, hit = branches["count"], branches["covered"]
        if not isinstance(count, int) or not isinstance(hit, int) or not 0 <= hit <= count:
            raise ValueError("Invalid branch counts")
        total += count
        covered += hit
        for file in unit["files"]:
            stats = file["summary"]["branches"]
            if stats["count"]:
                rows.append((file["filename"], stats["covered"], stats["count"]))
    if not total:
        raise ValueError("No branch data: run cargo llvm-cov with --branch on the pinned nightly")
    lines = [
        "## Rust branch coverage",
        "",
        f"**{covered / total:.2%}** ({covered:,} / {total:,} branch outcomes covered; "
        f"{total - covered:,} uncovered).",
        "",
        "| File | Covered / total branches | Branch coverage |",
        "| --- | ---: | ---: |",
    ]
    for filename, hit, count in sorted(rows):
        path = pathlib.Path(filename)
        try:
            path = path.relative_to(pathlib.Path.cwd())
        except ValueError:
            pass
        label = str(path).replace("|", "\\|").replace("\n", " ")
        lines.append(f"| {label} | {hit} / {count} | {hit / count:.2%} |")
    lines.extend([
        "", "Download the coverage artifact and open `html/index.html` for per-branch counts.",
        "This measures instrumented branch outcomes, not all execution paths or correctness.",
    ])
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    try:
        print(summarize(json.loads(pathlib.Path(sys.argv[1]).read_text())), end="")
    except (KeyError, TypeError, ValueError) as error:
        sys.exit(f"Invalid branch coverage report: {error}")
