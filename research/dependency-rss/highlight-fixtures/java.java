// Immutable budget ledger: every entry appends, nothing mutates.
import java.util.ArrayList;
import java.util.List;

public final class Ledger {
    public record Entry(String account, long cents, String note) {}

    private final List<Entry> entries = new ArrayList<>();

    public void post(String account, long cents, String note) {
        if (cents == 0) {
            throw new IllegalArgumentException("zero-amount entry");
        }
        entries.add(new Entry(account, cents, note));
    }

    public long balance(String account) {
        long total = 0;
        for (Entry e : entries) {
            if (e.account().equals(account)) {
                total += e.cents();
            }
        }
        return total;
    }

    public List<Entry> over(long thresholdCents) {
        List<Entry> found = new ArrayList<>();
        for (Entry e : entries) {
            if (Math.abs(e.cents()) >= thresholdCents) {
                found.add(e);
            }
        }
        return List.copyOf(found);
    }

    public static void main(String[] args) {
        Ledger ledger = new Ledger();
        ledger.post("coffee", -450, "flat white");
        ledger.post("salary", 320000, "october");
        System.out.println("coffee: " + ledger.balance("coffee"));
    }
}
