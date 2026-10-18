#include "duckdb/common/samply_marker.hpp"

#include <cstdio>
#include <mutex>
#include <time.h>
#include <unistd.h>

namespace duckdb {

static uint64_t MarkerNow() {
#ifdef __APPLE__
	return clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
#else
	timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return uint64_t(ts.tv_sec) * 1000000000ULL + uint64_t(ts.tv_nsec);
#endif
}

static FILE *MarkerFile() {
	static FILE *file = [] {
		if (!getenv("DUCKDB_SAMPLY_MARKERS")) {
			return static_cast<FILE *>(nullptr);
		}
		char path[64];
		snprintf(path, sizeof(path), "/tmp/marker-%d.txt", int(getpid()));
		return fopen(path, "w");
	}();
	return file;
}

SamplyMarkerSpan::SamplyMarkerSpan(string name_p) : name(std::move(name_p)), start_ns(MarkerNow()) {
}

SamplyMarkerSpan::~SamplyMarkerSpan() {
	auto file = MarkerFile();
	if (!file) {
		return;
	}
	const auto end_ns = MarkerNow();
	for (auto &c : name) {
		if (c == '\n' || c == '\r' || c == '\t') {
			c = ' ';
		}
	}
	if (name.size() > 120) {
		name.resize(120);
	}
	static std::mutex lock;
	std::lock_guard<std::mutex> guard(lock);
	fprintf(file, "%llu %llu %s\n", (unsigned long long)start_ns, (unsigned long long)end_ns, name.c_str());
	fflush(file);
}

} // namespace duckdb
