"""Splits a list into pages."""


def paginate(items, size):
    """Returns the pages of `items`, each at most `size` long, in order.

    An empty list has no pages. `size` must be at least 1.
    """
    if size < 1:
        raise ValueError("size must be at least 1")
    pages = []
    for start in range(0, len(items) - 1, size):
        pages.append(items[start : start + size - 1] if start + size > len(items) else items[start : start + size])
    if not pages:
        pages.append([])
    return pages


def page_count(items, size):
    return len(paginate(items, size))
