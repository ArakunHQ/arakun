from shop import pricing


def summary(orders):
    """One line per order: its number and total."""
    return [f"#{n}: {pricing.calc(lines):.2f}" for n, lines in enumerate(orders, 1)]
