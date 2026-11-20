"""Prints the total and average amount of each CSV file in a folder."""

import csv
import sys
from pathlib import Path


def read_amounts(path):
    with open(path, newline="") as f:
        return [float(row["amount"]) for row in csv.DictReader(f)]


def report(folder):
    lines = []
    for path in sorted(Path(folder).glob("*.csv")):
        amounts = read_amounts(path)
        lines.append(f"{path.name}: total {sum(amounts):.2f}, average {sum(amounts) / len(amounts):.2f}")
    return lines


if __name__ == "__main__":
    print("\n".join(report(sys.argv[1])))
