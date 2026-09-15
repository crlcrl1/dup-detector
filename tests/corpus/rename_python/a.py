def compute_total(items):
    total = 0
    for item in items:
        total = total + item * item
        if total > 100:
            total = total - 50
    return total
