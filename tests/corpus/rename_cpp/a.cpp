int compute_total(const std::vector<int>& items) {
    int sum = 0;
    for (int item : items) {
        sum = sum + item * item;
        if (sum > 100) {
            sum = sum - 50;
        }
    }
    return sum;
}
