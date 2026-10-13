// Bounded queue shared between the reader and serve threads.
#include <condition_variable>
#include <cstddef>
#include <mutex>
#include <optional>
#include <queue>
#include <string>

template <typename T> class BoundedQueue {
  public:
    explicit BoundedQueue(std::size_t cap) : cap_(cap) {}

    void push(T value) {
        std::unique_lock<std::mutex> lock(mu_);
        not_full_.wait(lock, [&] { return queue_.size() < cap_ || closed_; });
        if (closed_) {
            return;
        }
        queue_.push(std::move(value));
        not_empty_.notify_one();
    }

    std::optional<T> pop() {
        std::unique_lock<std::mutex> lock(mu_);
        not_empty_.wait(lock, [&] { return !queue_.empty() || closed_; });
        if (queue_.empty()) {
            return std::nullopt;
        }
        T value = std::move(queue_.front());
        queue_.pop();
        not_full_.notify_one();
        return value;
    }

    void close() {
        std::lock_guard<std::mutex> lock(mu_);
        closed_ = true;
        not_empty_.notify_all();
        not_full_.notify_all();
    }

  private:
    std::size_t cap_;
    bool closed_ = false;
    std::mutex mu_;
    std::condition_variable not_empty_;
    std::condition_variable not_full_;
    std::queue<T> queue_;
};
