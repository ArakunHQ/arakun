from shop.pricing import calc


class Cart:
    def __init__(self):
        self.lines = []

    def add(self, price, qty=1):
        self.lines.append((price, qty))

    def total(self, member=False):
        return calc(self.lines, d=0.1 if member else 0.0)
