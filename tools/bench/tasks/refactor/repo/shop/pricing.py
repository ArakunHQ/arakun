def calc(lines, d=0.0):
    """The total of (price, quantity) lines, less a discount between 0 and 1."""
    subtotal = sum(price * qty for price, qty in lines)
    return round(subtotal * (1 - d), 2)
