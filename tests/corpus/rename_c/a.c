int compute_total(const int *items, int count) {
    int sum = 0;
    for (int i = 0; i < count; i++) {
        int value = items[i];
        sum = sum + value * value;
        if (sum > 100) {
            sum = sum - 50;
        }
    }
    return sum;
}
