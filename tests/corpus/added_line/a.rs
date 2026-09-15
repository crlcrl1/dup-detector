fn compute_total(items: Vec<i32>) -> i32 {
    let mut sum = 0;
    for item in items {
        sum = sum + item * item;
    }
    return sum;
}
