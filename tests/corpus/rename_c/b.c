int calc_sum(const int *values, int length) {
    int total = 0;
    for (int j = 0; j < length; j++) {
        int entry = values[j];
        total = total + entry * entry;
        if (total > 100) {
            total = total - 50;
        }
    }
    return total;
}
