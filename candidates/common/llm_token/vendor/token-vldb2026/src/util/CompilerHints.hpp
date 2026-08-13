#pragma once
//---------------------------------------------------------------------------
#include <cassert>
#include <cstdint>
#include <utility>
//---------------------------------------------------------------------------
// Static Global Token Table
// (c) 2025 Tobias Schmidt
//---------------------------------------------------------------------------
/// Skip the creation of some template specializations for faster compilation
#define FASTCOMP 1
//---------------------------------------------------------------------------
/// Compiler hints for inlining
#ifdef NDEBUG
#ifndef forceinline
#define forceinline inline __attribute__((always_inline))
#endif
#else
#ifndef forceinline
#define forceinline
#endif
#endif
//---------------------------------------------------------------------------
/// Compiler hint to unroll loops
#ifdef NDEBUG
#ifndef unroll_loops
#define unroll_loops __attribute__((optimize("unroll-loops")))
#endif
#else
#ifndef unroll_loops
#define unroll_loops
#endif
#endif
//---------------------------------------------------------------------------
/// Compiler hint for likely branch
#ifndef likely
#define likely(expr) __builtin_expect((expr), 1)
#endif
//---------------------------------------------------------------------------
/// Compiler hint for unlikely branches
#ifndef unlikely
#define unlikely(expr) __builtin_expect((expr), 0)
#endif
//---------------------------------------------------------------------------
/// A 128bit data type
struct data128_t {
   uint64_t values[2];
};
//---------------------------------------------------------------------------
/// A 128bit signed integer type
using int128_t = __int128;
//---------------------------------------------------------------------------
/// A 128bit unsigned integer type
using uint128_t = unsigned __int128;
//---------------------------------------------------------------------------
/// Wrapper for unaligned data types
template <class T>
struct [[gnu::packed, gnu::may_alias]] unaligned {
   T value;
   /// Load the value
   constexpr T get() const noexcept { return value; }
   /// Implicit conversion to the value
   constexpr operator T() const noexcept { return value; }
   /// Default constructor
   constexpr unaligned() noexcept = default;
   /// Implicit converting constructor
   constexpr unaligned(T value) noexcept : value(value) {}
   /// Get the potentially unaligned address
   void* getPtr() noexcept { return this; }
   /// Get the potentially unaligned address
   const void* getPtr() const noexcept { return this; }
};
static_assert(alignof(unaligned<void*>) == 1, "unaligned should be packed");
//---------------------------------------------------------------------------
/// Unaligned load
template <class T>
forceinline T unalignedLoad(const void* ptr) noexcept { return static_cast<const unaligned<T>*>(ptr)->value; }
/// Unaligned store
template <class T>
forceinline void unalignedStore(void* ptr, T value) noexcept { static_cast<unaligned<T>*>(ptr)->value = value; }
//---------------------------------------------------------------------------
