#pragma once

// The timer, the machine name, the file readers and the CSV writer the
// sweeps and tools share.

#include <chrono>
#include <cmath>
#include <cstdio>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <map>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace fsst::search::prefilter::scan::bench {

constexpr int PASSES = 5;

inline std::string read_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) throw std::runtime_error(path.string() + ": cannot open");
    return std::string((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
}

inline std::vector<std::string> split(const std::string& s, char sep) {
    std::vector<std::string> out;
    size_t at = 0;
    while (true) {
        size_t end = s.find(sep, at);
        out.push_back(s.substr(at, end == std::string::npos ? std::string::npos : end - at));
        if (end == std::string::npos) return out;
        at = end + 1;
    }
}

// Seconds of the fastest of PASSES runs after one untimed. A run the clock
// cannot resolve is repeated inside the timed region until it can.
template <typename Run>
double best(Run run) {
    constexpr double FLOOR = 1e-6;
    run();
    size_t reps = 1;
    while (true) {
        double fastest = 1e300;
        for (int pass = 0; pass < PASSES; ++pass) {
            auto start = std::chrono::steady_clock::now();
            for (size_t r = 0; r < reps; ++r) run();
            double seconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - start).count();
            if (seconds < fastest) fastest = seconds;
        }
        if (fastest > FLOOR || reps == (size_t{1} << 20)) return fastest / static_cast<double>(reps);
        reps *= 2;
    }
}

inline std::string shell(const char* command) {
    std::string out;
    if (FILE* pipe = popen(command, "r")) {
        char buf[256];
        while (fgets(buf, sizeof buf, pipe)) out += buf;
        pclose(pipe);
    }
    return out;
}

inline std::string labelled(const std::string& text, const std::string& label) {
    size_t at = text.find(label);
    if (at == std::string::npos) return "";
    size_t colon = text.find(':', at);
    size_t end = text.find('\n', colon);
    return colon == std::string::npos ? "" : text.substr(colon + 1, end - colon - 1);
}

inline std::string collapse(std::string s) {
    for (char& c : s)
        if (c == ',' || c == '\n' || c == '\t') c = ' ';
    std::string out;
    for (char c : s) {
        if (c == ' ' && (out.empty() || out.back() == ' ')) continue;
        out.push_back(c);
    }
    while (!out.empty() && out.back() == ' ') out.pop_back();
    return out;
}

// "<arch> <cpu model> <isa>", the key a fit groups rows by.
inline std::string machine() {
    std::string model = labelled(shell("cat /proc/cpuinfo 2>/dev/null"), "model name");
    if (model.empty()) model = labelled(shell("lscpu 2>/dev/null"), "Model name");
    if (model.empty()) model = shell("sysctl -n machdep.cpu.brand_string 2>/dev/null");
    if (collapse(model).empty()) model = "unknown cpu";
#if defined(__aarch64__)
    const char* arch = "aarch64";
    const char* isa = "neon";
#elif defined(__AVX512BW__)
    const char* arch = "x86_64";
    const char* isa = "avx512bw";
#elif defined(__AVX2__)
    const char* arch = "x86_64";
    const char* isa = "avx2";
#else
    const char* arch = "x86_64";
    const char* isa = "scalar";
#endif
    return collapse(std::string(arch) + " " + model + " " + isa);
}

inline std::string number(double v) {
    char buf[32];
    std::snprintf(buf, sizeof buf, "%.15g", v);
    return buf;
}

inline std::filesystem::path output_dir() { return std::filesystem::path(BENCH_DIR) / "output"; }

inline std::string timestamp() {
    std::time_t now = std::time(nullptr);
    char buf[32];
    std::strftime(buf, sizeof buf, "%Y-%m-%d_%H-%M-%S", std::localtime(&now));
    return buf;
}

// `output/<prefix>_<stamp>.csv` from a header line and one line per row.
inline std::filesystem::path write_csv(const std::string& prefix, const std::string& header,
                                       const std::vector<std::string>& rows) {
    std::filesystem::create_directories(output_dir());
    std::filesystem::path path = output_dir() / (prefix + "_" + timestamp() + ".csv");
    std::ofstream out(path);
    out << header << '\n';
    for (const std::string& row : rows) out << row << '\n';
    return path;
}

// The newest `output/<prefix>_*.csv`, or `given` when it is not empty.
inline std::filesystem::path csv_source(const std::string& prefix, const std::string& given) {
    if (!given.empty()) return given;
    std::filesystem::path newest;
    for (auto& entry : std::filesystem::directory_iterator(output_dir())) {
        std::string name = entry.path().filename().string();
        if (name.rfind(prefix + "_", 0) == 0 && name.size() > 4 && name.compare(name.size() - 4, 4, ".csv") == 0 &&
            (newest.empty() || entry.path().filename() > newest.filename()))
            newest = entry.path();
    }
    if (newest.empty()) {
        std::fprintf(stderr, "run the %s sweep first\n", prefix.c_str());
        std::exit(1);
    }
    return newest;
}

// A CSV as one map per row, keyed by the header.
inline std::vector<std::map<std::string, std::string>> read_csv(const std::filesystem::path& path) {
    std::ifstream in(path);
    if (!in) {
        std::fprintf(stderr, "cannot read %s\n", path.c_str());
        std::exit(1);
    }
    std::string line;
    std::getline(in, line);
    std::vector<std::string> header = split(line, ',');
    std::vector<std::map<std::string, std::string>> rows;
    while (std::getline(in, line)) {
        if (line.empty()) continue;
        std::vector<std::string> field = split(line, ',');
        std::map<std::string, std::string> row;
        for (size_t i = 0; i < header.size() && i < field.size(); ++i) row[header[i]] = field[i];
        rows.push_back(std::move(row));
    }
    return rows;
}

// `a + b*x` minimising the relative error: a model is read as a rate.
inline std::pair<double, double> line_fit(const std::vector<std::pair<double, double>>& point) {
    double sw = 0, swx = 0, swy = 0, swxx = 0, swxy = 0;
    for (auto [x, y] : point) {
        double w = 1.0 / (y * y);
        sw += w;
        swx += w * x;
        swy += w * y;
        swxx += w * x * x;
        swxy += w * x * y;
    }
    // Relative, because an FMA contraction leaves a residual where the exact
    // difference is zero, and the slope would then be noise over noise.
    double spread = sw * swxx - swx * swx;
    if (std::fabs(spread) <= 1e-9 * sw * swxx) return {swy / sw, 0.0};
    double slope = (sw * swxy - swx * swy) / spread;
    return {(swy - slope * swx) / sw, slope};
}

// A slope through the origin, for what one term adds to a model fitted without it.
inline double slope_fit(const std::vector<std::pair<double, double>>& point) {
    double swxy = 0, swxx = 0;
    for (auto [x, y] : point) {
        double w = 1.0 / (y * y);
        swxy += w * x * y;
        swxx += w * x * x;
    }
    return std::fabs(swxx) < 1e-12 ? 0.0 : swxy / swxx;
}

// Mean relative error of `pred` over the points, in percent.
template <typename Pred>
inline double rel_error(const std::vector<std::pair<double, double>>& point, Pred pred) {
    if (point.empty()) return NAN;
    double sum = 0;
    for (auto [x, y] : point) sum += std::fabs(pred(x) / y - 1.0);
    return 100.0 * sum / point.size();
}

}  // namespace fsst::search::prefilter::scan::bench
