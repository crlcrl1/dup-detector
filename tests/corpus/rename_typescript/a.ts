function computeTotal(items: number[]): number {
    let sum = 0;
    for (const item of items) {
        sum = sum + item * item;
        if (sum > 100) {
            sum = sum - 50;
        }
    }
    return sum;
}
