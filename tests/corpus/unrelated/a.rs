fn compute_total(items: Vec<i32>) -> i32 {
    let mut sum = 0;
    for item in items {
        sum = sum + item * item;
        if sum > 100 {
            sum = sum - 50;
        }
    }
    return sum;
}

fn compute_average(items: Vec<i32>) -> f64 {
    if items.is_empty() {
        return 0.0;
    }
    let mut sum = 0;
    for item in &items {
        sum = sum + item;
    }
    return sum as f64 / items.len() as f64;
}
