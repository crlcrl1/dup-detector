int calc_sum(const std::vector<int>& values) {
    int total = 0;
    for (int value : values) {
        total = total + value * value;
        if (total > 100) {
            total = total - 50;
        }
    }
    return total;
}
