def calc_sum(values):
    result = 0
    for value in values:
        result = result + value * value
        if result > 100:
            result = result - 50
    return result
