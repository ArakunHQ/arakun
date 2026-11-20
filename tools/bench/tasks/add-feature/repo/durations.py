"""Durations as people type them."""


def parse_duration(text):
    """Returns the seconds in a duration such as "1h30m", "45s", "2d 4h" or "90m".

    Units: d (days), h (hours), m (minutes), s (seconds). Parts may be separated
    by spaces, are case-insensitive, and each unit appears at most once, in
    that order (largest first). A bare number is seconds. Anything else
    (empty text, an unknown unit, a repeated or out-of-order unit, a negative
    number) raises ValueError.
    """
    raise NotImplementedError
