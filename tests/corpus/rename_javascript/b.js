function calcSum(values) {
    let total = 0;
    for (const value of values) {
        total = total + value * value;
        if (total > 100) {
            total = total - 50;
        }
    }
    return total;
}
