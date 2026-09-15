fn calc_sum(values: Vec<i32>) -> i32 {
    let mut total = 0;
    for value in values {
        total = total + value * value;
        if total > 100 {
            total = total - 50;
        }
    }
    return total;
}
