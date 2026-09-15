fn compute_total(items: Vec<i32>) -> i32 {
    let mut sum = 7;
    for item in items {
        sum = sum + item * item;
        if sum > 200 {
            sum = sum - 25;
        }
    }
    return sum;
}
