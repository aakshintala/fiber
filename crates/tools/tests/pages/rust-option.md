# Option in std::option - Rust

[Skip to main content](#main-content)

## [Option](#)

[![logo](../../static.files/rust-logo-9a9549ea.svg)](../../std/index.html)

## [std](../../std/index.html)1.99.0

(b940084d7 2026-09-28)

## [Option](#)

### [Variants](#variants)

- [None](#variant.None)
- [Some](#variant.Some)

### [Methods](#implementations)

- [and](#method.and)
- [and_then](#method.and_then)
- [as_deref](#method.as_deref)
- [as_deref_mut](#method.as_deref_mut)
- [as_mut](#method.as_mut)
- [as_mut_slice](#method.as_mut_slice)
- [as_pin_mut](#method.as_pin_mut)
- [as_pin_ref](#method.as_pin_ref)
- [as_ref](#method.as_ref)
- [as_slice](#method.as_slice)
- [cloned](#method.cloned)
- [cloned](#method.cloned-1)
- [copied](#method.copied)
- [copied](#method.copied-1)
- [expect](#method.expect)
- [filter](#method.filter)
- [flatten](#method.flatten)
- [flatten_mut](#method.flatten_mut)
- [flatten_ref](#method.flatten_ref)
- [flatten_ref](#method.flatten_ref-1)
- [get_or_insert](#method.get_or_insert)
- [get_or_insert_default](#method.get_or_insert_default)
- [get_or_insert_with](#method.get_or_insert_with)
- [get_or_try_insert_with](#method.get_or_try_insert_with)
- [insert](#method.insert)
- [inspect](#method.inspect)
- [into_flat_iter](#method.into_flat_iter)
- [is_none](#method.is_none)
- [is_none_or](#method.is_none_or)
- [is_some](#method.is_some)
- [is_some_and](#method.is_some_and)
- [iter](#method.iter)
- [iter_mut](#method.iter_mut)
- [map](#method.map)
- [map_or](#method.map_or)
- [map_or_default](#method.map_or_default)
- [map_or_else](#method.map_or_else)
- [ok_or](#method.ok_or)
- [ok_or_else](#method.ok_or_else)
- [or](#method.or)
- [or_else](#method.or_else)
- [reduce](#method.reduce)
- [replace](#method.replace)
- [take](#method.take)
- [take_if](#method.take_if)
- [transpose](#method.transpose)
- [unwrap](#method.unwrap)
- [unwrap_or](#method.unwrap_or)
- [unwrap_or_default](#method.unwrap_or_default)
- [unwrap_or_else](#method.unwrap_or_else)
- [unwrap_unchecked](#method.unwrap_unchecked)
- [unzip](#method.unzip)
- [xor](#method.xor)
- [zip](#method.zip)
- [zip_with](#method.zip_with)

### [Trait Implementations](#trait-implementations)

- [Clone](#impl-Clone-for-Option%3CT%3E)
- [CloneFromCell](#impl-CloneFromCell-for-Option%3CT%3E)
- [Copy](#impl-Copy-for-Option%3CT%3E)
- [Debug](#impl-Debug-for-Option%3CT%3E)
- [Default](#impl-Default-for-Option%3CT%3E)
- [Eq](#impl-Eq-for-Option%3CT%3E)
- [From<&'a Option<T>>](#impl-From%3C%26Option%3CT%3E%3E-for-Option%3C%26T%3E)
- [From<&'a mut Option<T>>](#impl-From%3C%26mut+Option%3CT%3E%3E-for-Option%3C%26mut+T%3E)
- [From<T>](#impl-From%3CT%3E-for-Option%3CT%3E)
- [FromIterator<Option<A>>](#impl-FromIterator%3COption%3CA%3E%3E-for-Option%3CV%3E)
- [FromResidual<Option<Infallible>>](#impl-FromResidual%3COption%3CInfallible%3E%3E-for-Option%3CT%3E)
- [FromResidual<Yeet<()>>](#impl-FromResidual%3CYeet%3C()%3E%3E-for-Option%3CT%3E)
- [Hash](#impl-Hash-for-Option%3CT%3E)
- [IntoIterator](#impl-IntoIterator-for-%26Option%3CT%3E)
- [IntoIterator](#impl-IntoIterator-for-%26mut+Option%3CT%3E)
- [IntoIterator](#impl-IntoIterator-for-Option%3CT%3E)
- [Ord](#impl-Ord-for-Option%3CT%3E)
- [PartialEq](#impl-PartialEq-for-Option%3CT%3E)
- [PartialOrd](#impl-PartialOrd-for-Option%3CT%3E)
- [Product<Option<U>>](#impl-Product%3COption%3CU%3E%3E-for-Option%3CT%3E)
- [Residual<T>](#impl-Residual%3CT%3E-for-Option%3CInfallible%3E)
- [StructuralPartialEq](#impl-StructuralPartialEq-for-Option%3CT%3E)
- [Sum<Option<U>>](#impl-Sum%3COption%3CU%3E%3E-for-Option%3CT%3E)
- [Try](#impl-Try-for-Option%3CT%3E)
- [UseCloned](#impl-UseCloned-for-Option%3CT%3E)

### [Auto Trait Implementations](#synthetic-implementations)

- [Freeze](#impl-Freeze-for-Option%3CT%3E)
- [RefUnwindSafe](#impl-RefUnwindSafe-for-Option%3CT%3E)
- [Send](#impl-Send-for-Option%3CT%3E)
- [Sync](#impl-Sync-for-Option%3CT%3E)
- [Unpin](#impl-Unpin-for-Option%3CT%3E)
- [UnsafeUnpin](#impl-UnsafeUnpin-for-Option%3CT%3E)
- [UnwindSafe](#impl-UnwindSafe-for-Option%3CT%3E)

### [Blanket Implementations](#blanket-implementations)

- [Any](#impl-Any-for-T)
- [Borrow<T>](#impl-Borrow%3CT%3E-for-T)
- [BorrowMut<T>](#impl-BorrowMut%3CT%3E-for-T)
- [CloneToUninit](#impl-CloneToUninit-for-T)
- [From<!>](#impl-From%3C!%3E-for-T)
- [From<T>](#impl-From%3CT%3E-for-T)
- [Into<U>](#impl-Into%3CU%3E-for-T)
- [ToOwned](#impl-ToOwned-for-T)
- [TryFrom<U>](#impl-TryFrom%3CU%3E-for-T)
- [TryInto<U>](#impl-TryInto%3CU%3E-for-T)

## [In std::option](index.html)

[std](../index.html)::[option](index.html)

# Enum Option Copy item path

1.0.0 · [Source](../../src/core/option.rs.html#598)

```
pub enum Option<T> {
    None,
    Some(T),
}
```

Expand description

The `Option` type. See [the module level documentation](index.html) for more.

## Variants[§](#variants)

[§](#variant.None)1.0.0

### None

No value.

[§](#variant.Some)1.0.0

### Some(T)

Some value of type `T`.

## Implementations[§](#implementations)

[Source](../../src/core/option.rs.html#613)[§](#impl-Option%3CT%3E)

### impl<T> [Option](enum.Option.html)<T>

1.0.0 (const: 1.48.0) · [Source](../../src/core/option.rs.html#633)

#### pub const fn [is_some](#method.is_some)(&self) -> [bool](../primitive.bool.html)

Returns `true` if the option is a [`Some`](enum.Option.html#variant.Some) value.

##### [§](#examples)Examples

```
let x: Option<u32> = Some(2);
assert_eq!(x.is_some(), true);

let x: Option<u32> = None;
assert_eq!(x.is_some(), false);
```

1.70.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#659)

#### pub fn [is_some_and](#method.is_some_and)(self, f: impl [FnOnce](../ops/trait.FnOnce.html)(T) -> [bool](../primitive.bool.html)) -> [bool](../primitive.bool.html)

Returns `true` if the option is a [`Some`](enum.Option.html#variant.Some) and the value inside of it matches a predicate.

##### [§](#examples-1)Examples

```
let x: Option<u32> = Some(2);
assert_eq!(x.is_some_and(|x| x > 1), true);

let x: Option<u32> = Some(0);
assert_eq!(x.is_some_and(|x| x > 1), false);

let x: Option<u32> = None;
assert_eq!(x.is_some_and(|x| x > 1), false);

let x: Option<String> = Some("ownership".to_string());
assert_eq!(x.as_ref().is_some_and(|x| x.len() > 1), true);
println!("still alive {:?}", x);
```

1.0.0 (const: 1.48.0) · [Source](../../src/core/option.rs.html#682)

#### pub const fn [is_none](#method.is_none)(&self) -> [bool](../primitive.bool.html)

Returns `true` if the option is a [`None`](enum.Option.html#variant.None) value.

##### [§](#examples-2)Examples

```
let x: Option<u32> = Some(2);
assert_eq!(x.is_none(), false);

let x: Option<u32> = None;
assert_eq!(x.is_none(), true);
```

1.82.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#708)

#### pub fn [is_none_or](#method.is_none_or)(self, f: impl [FnOnce](../ops/trait.FnOnce.html)(T) -> [bool](../primitive.bool.html)) -> [bool](../primitive.bool.html)

Returns `true` if the option is a [`None`](enum.Option.html#variant.None) or the value inside of it matches a predicate.

##### [§](#examples-3)Examples

```
let x: Option<u32> = Some(2);
assert_eq!(x.is_none_or(|x| x > 1), true);

let x: Option<u32> = Some(0);
assert_eq!(x.is_none_or(|x| x > 1), false);

let x: Option<u32> = None;
assert_eq!(x.is_none_or(|x| x > 1), true);

let x: Option<String> = Some("ownership".to_string());
assert_eq!(x.as_ref().is_none_or(|x| x.len() > 1), true);
println!("still alive {:?}", x);
```

1.0.0 (const: 1.48.0) · [Source](../../src/core/option.rs.html#742)

#### pub const fn [as_ref](#method.as_ref)(&self) -> [Option](enum.Option.html)<[&T](../primitive.reference.html)>

Converts from `&Option<T>` to `Option<&T>`.

##### [§](#examples-4)Examples

Calculates the length of an `Option<[String](../../std/string/struct.String.html)>` as an `Option<[usize](../primitive.usize.html)>` without moving the [`String`](../../std/string/struct.String.html). The [`map`](enum.Option.html#method.map) method takes the `self` argument by value, consuming the original, so this technique uses `as_ref` to first take an `Option` to a reference to the value inside the original.

```
let text: Option<String> = Some("Hello, world!".to_string());
// First, cast `Option<String>` to `Option<&String>` with `as_ref`,
// then consume *that* with `map`, leaving `text` on the stack.
let text_length: Option<usize> = text.as_ref().map(|s| s.len());
println!("still can print text: {text:?}");
```

1.0.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#764)

#### pub const fn [as_mut](#method.as_mut)(&mut self) -> [Option](enum.Option.html)<[&mut T](../primitive.reference.html)>

Converts from `&mut Option<T>` to `Option<&mut T>`.

##### [§](#examples-5)Examples

```
let mut x = Some(2);
match x.as_mut() {
    Some(v) => *v = 42,
    None => {},
}
assert_eq!(x, Some(42));
```

1.33.0 (const: 1.84.0) · [Source](../../src/core/option.rs.html#778)

#### pub const fn [as_pin_ref](#method.as_pin_ref)(self: [Pin](../pin/struct.Pin.html)<&[Option](enum.Option.html)<T>>) -> [Option](enum.Option.html)<[Pin](../pin/struct.Pin.html)<[&T](../primitive.reference.html)>>

Converts from `[Pin](../pin/struct.Pin.html)<[&](../primitive.reference.html)Option<T>>` to `Option<[Pin](../pin/struct.Pin.html)<[&](../primitive.reference.html)T>>`.

1.33.0 (const: 1.84.0) · [Source](../../src/core/option.rs.html#795)

#### pub const fn [as_pin_mut](#method.as_pin_mut)(self: [Pin](../pin/struct.Pin.html)<&mut [Option](enum.Option.html)<T>>) -> [Option](enum.Option.html)<[Pin](../pin/struct.Pin.html)<[&mut T](../primitive.reference.html)>>

Converts from `[Pin](../pin/struct.Pin.html)<[&mut](../primitive.reference.html) Option<T>>` to `Option<[Pin](../pin/struct.Pin.html)<[&mut](../primitive.reference.html) T>>`.

1.75.0 (const: 1.84.0) · [Source](../../src/core/option.rs.html#842)

#### pub const fn [as_slice](#method.as_slice)(&self) -> &[[T]](../primitive.slice.html)

Returns a slice of the contained value, if any. If this is `None`, an empty slice is returned. This can be useful to have a single type of iterator over an `Option` or slice.

Note: Should you have an `Option<&T>` and wish to get a slice of `T`, you can unpack it via `opt.map_or(&[], std::slice::from_ref)`.

##### [§](#examples-6)Examples

```
assert_eq!(
    [Some(1234).as_slice(), None.as_slice()],
    [&[1234][..], &[][..]],
);
```

The inverse of this function is (discounting borrowing) [`[_]::first`](../primitive.slice.html#method.first):

```
for i in [Some(1234_u16), None] {
    assert_eq!(i.as_ref(), i.as_slice().first());
}
```

1.75.0 (const: 1.84.0) · [Source](../../src/core/option.rs.html#897)

#### pub const fn [as_mut_slice](#method.as_mut_slice)(&mut self) -> &mut [[T]](../primitive.slice.html)

Returns a mutable slice of the contained value, if any. If this is `None`, an empty slice is returned. This can be useful to have a single type of iterator over an `Option` or slice.

Note: Should you have an `Option<&mut T>` instead of a `&mut Option<T>`, which this method takes, you can obtain a mutable slice via `opt.map_or(&mut [], std::slice::from_mut)`.

##### [§](#examples-7)Examples

```
assert_eq!(
    [Some(1234).as_mut_slice(), None.as_mut_slice()],
    [&mut [1234][..], &mut [][..]],
);
```

The result is a mutable slice of zero or one items that points into our original `Option`:

```
let mut x = Some(1234);
x.as_mut_slice()[0] += 1;
assert_eq!(x, Some(1235));
```

The inverse of this method (discounting borrowing) is [`[_]::first_mut`](../primitive.slice.html#method.first_mut):

```
assert_eq!(Some(123).as_mut_slice().first_mut(), Some(&mut 123))
```

1.0.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#966)

#### pub const fn [expect](#method.expect)(self, msg: &[str](../primitive.str.html)) -> T

Returns the contained [`Some`](enum.Option.html#variant.Some) value, consuming the `self` value.

##### [§](#panics)Panics

Panics if the value is a [`None`](enum.Option.html#variant.None) with a custom panic message provided by `msg`.

##### [§](#examples-8)Examples

```
let x = Some("value");
assert_eq!(x.expect("fruits are healthy"), "value");
```

[ⓘ](#)

```
let x: Option<&str> = None;
x.expect("fruits are healthy"); // panics with `fruits are healthy`
```

##### [§](#recommended-message-style)Recommended Message Style

We recommend that `expect` messages are used to describe the reason you _expect_ the `Option` should be `Some`.

[ⓘ](#)

```
let item = slice.get(0)
    .expect("slice should not be empty");
```

**Hint**: If you’re having trouble remembering how to phrase expect error messages remember to focus on the word “should” as in “env variable should be set by blah” or “the given binary should be available and executable by the current user”.

For more detail on expect message styles and the reasoning behind our recommendation please refer to the section on [“Common Message Styles”](../../std/error/index.html#common-message-styles) in the [`std::error`](../../std/error/index.html) module docs.

1.0.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#1011)

#### pub const fn [unwrap](#method.unwrap)(self) -> T

Returns the contained [`Some`](enum.Option.html#variant.Some) value, consuming the `self` value.

Because this function may panic, its use is generally discouraged. Panics are meant for unrecoverable errors, and [may abort the entire program](https://doc.rust-lang.org/book/ch09-01-unrecoverable-errors-with-panic.html).

Instead, prefer to use pattern matching and handle the [`None`](enum.Option.html#variant.None) case explicitly, or call [`unwrap_or`](enum.Option.html#method.unwrap_or), [`unwrap_or_else`](enum.Option.html#method.unwrap_or_else), or [`unwrap_or_default`](enum.Option.html#method.unwrap_or_default). In functions returning `Option`, you can use [the `?` (try) operator](https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html#where-the--operator-can-be-used).

##### [§](#panics-1)Panics

Panics if the self value equals [`None`](enum.Option.html#variant.None).

##### [§](#examples-9)Examples

```
let x = Some("air");
assert_eq!(x.unwrap(), "air");
```

[ⓘ](#)

```
let x: Option<&str> = None;
assert_eq!(x.unwrap(), "air"); // fails
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1036-1038)

#### pub fn [unwrap_or](#method.unwrap_or)(self, default: T) -> T

Returns the contained [`Some`](enum.Option.html#variant.Some) value or a provided default.

Arguments passed to `unwrap_or` are eagerly evaluated; if you are passing the result of a function call, it is recommended to use [`unwrap_or_else`](enum.Option.html#method.unwrap_or_else), which is lazily evaluated.

##### [§](#examples-10)Examples

```
assert_eq!(Some("car").unwrap_or("bike"), "car");
assert_eq!(None.unwrap_or("bike"), "bike");
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1059-1061)

#### pub fn [unwrap_or_else](#method.unwrap_or_else)<F>(self, f: F) -> T

where F: [FnOnce](../ops/trait.FnOnce.html)() -> T,

Returns the contained [`Some`](enum.Option.html#variant.Some) value or computes it from a closure.

##### [§](#examples-11)Examples

```
let k = 10;
assert_eq!(Some(4).unwrap_or_else(|| 2 * k), 4);
assert_eq!(None.unwrap_or_else(|| 2 * k), 20);
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1091-1093)

#### pub fn [unwrap_or_default](#method.unwrap_or_default)(self) -> T

where T: [Default](../default/trait.Default.html),

Returns the contained [`Some`](enum.Option.html#variant.Some) value or a default.

Consumes the `self` argument then, if [`Some`](enum.Option.html#variant.Some), returns the contained value, otherwise if [`None`](enum.Option.html#variant.None), returns the [default value](../default/trait.Default.html#tymethod.default) for that type.

##### [§](#examples-12)Examples

```
let x: Option<u32> = None;
let y: Option<u32> = Some(12);

assert_eq!(x.unwrap_or_default(), 0);
assert_eq!(y.unwrap_or_default(), 12);
```

1.58.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#1126)

#### pub const unsafe fn [unwrap_unchecked](#method.unwrap_unchecked)(self) -> T

Returns the contained [`Some`](enum.Option.html#variant.Some) value, consuming the `self` value, without checking that the value is not [`None`](enum.Option.html#variant.None).

##### [§](#safety)Safety

Calling this method on [`None`](enum.Option.html#variant.None) is _[undefined behavior](https://doc.rust-lang.org/reference/behavior-considered-undefined.html)_.

##### [§](#examples-13)Examples

```
let x = Some("air");
assert_eq!(unsafe { x.unwrap_unchecked() }, "air");
```

```
let x: Option<&str> = None;
assert_eq!(unsafe { x.unwrap_unchecked() }, "air"); // Undefined behavior!
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1158-1160)

#### pub fn [map](#method.map)<U, F>(self, f: F) -> [Option](enum.Option.html)<U>

where F: [FnOnce](../ops/trait.FnOnce.html)(T) -> U,

Maps an `Option<T>` to `Option<U>` by applying a function to a contained value (if `Some`) or returns `None` (if `None`).

##### [§](#examples-14)Examples

Calculates the length of an `Option<[String](../../std/string/struct.String.html)>` as an `Option<[usize](../primitive.usize.html)>`, consuming the original:

```
let maybe_some_string = Some(String::from("Hello, World!"));
// `Option::map` takes self *by value*, consuming `maybe_some_string`
let maybe_some_len = maybe_some_string.map(|s| s.len());
assert_eq!(maybe_some_len, Some(13));

let x: Option<&str> = None;
assert_eq!(x.map(|s| s.len()), None);
```

1.76.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1189-1191)

#### pub fn [inspect](#method.inspect)<F>(self, f: F) -> [Option](enum.Option.html)<T>

where F: [FnOnce](../ops/trait.FnOnce.html)([&T](../primitive.reference.html)),

Calls a function with a reference to the contained value if [`Some`](enum.Option.html#variant.Some).

Returns the original option.

##### [§](#examples-15)Examples

```
let list = vec![1, 2, 3];

// prints "got: 2"
let x = list
    .get(1)
    .inspect(|x| println!("got: {x}"))
    .expect("list should be long enough");

// prints nothing
list.get(5).inspect(|x| println!("got: {x}"));
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1222-1225)

#### pub fn [map_or](#method.map_or)<U, F>(self, default: U, f: F) -> U

where F: [FnOnce](../ops/trait.FnOnce.html)(T) -> U,

Returns the provided default result (if none), or applies a function to the contained value (if any).

Arguments passed to `map_or` are eagerly evaluated; if you are passing the result of a function call, it is recommended to use [`map_or_else`](enum.Option.html#method.map_or_else), which is lazily evaluated.

##### [§](#examples-16)Examples

```
let x = Some("foo");
assert_eq!(x.map_or(42, |v| v.len()), 3);

let x: Option<&str> = None;
assert_eq!(x.map_or(42, |v| v.len()), 42);
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1269-1272)

#### pub fn [map_or_else](#method.map_or_else)<U, D, F>(self, default: D, f: F) -> U

where D: [FnOnce](../ops/trait.FnOnce.html)() -> U, F: [FnOnce](../ops/trait.FnOnce.html)(T) -> U,

Computes a default function result (if none), or applies a different function to the contained value (if any).

##### [§](#basic-examples)Basic examples

```
let k = 21;

let x = Some("foo");
assert_eq!(x.map_or_else(|| 2 * k, |v| v.len()), 3);

let x: Option<&str> = None;
assert_eq!(x.map_or_else(|| 2 * k, |v| v.len()), 42);
```

##### [§](#handling-a-result-based-fallback)Handling a Result-based fallback

A somewhat common occurrence when dealing with optional values in combination with [`Result<T, E>`](../result/enum.Result.html) is the case where one wants to invoke a fallible fallback if the option is not present. This example parses a command line argument (if present), or the contents of a file to an integer. However, unlike accessing the command line argument, reading the file is fallible, so it must be wrapped with `Ok`.

```
let v: u64 = std::env::args()
   .nth(1)
   .map_or_else(|| std::fs::read_to_string("/etc/someconfig.conf"), Ok)?
   .parse()?;
```

1.98.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1298-1301)

#### pub fn [map_or_default](#method.map_or_default)<U, F>(self, f: F) -> U

where U: [Default](../default/trait.Default.html), F: [FnOnce](../ops/trait.FnOnce.html)(T) -> U,

Maps an `Option<T>` to a `U` by applying function `f` to the contained value if the option is [`Some`](enum.Option.html#variant.Some), otherwise if [`None`](enum.Option.html#variant.None), returns the [default value](../default/trait.Default.html#tymethod.default) for the type `U`.

##### [§](#examples-17)Examples

```
let x: Option<&str> = Some("hi");
let y: Option<&str> = None;

assert_eq!(x.map_or_default(|x| x.len()), 2);
assert_eq!(y.map_or_default(|y| y.len()), 0);
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1333)

#### pub fn [ok_or](#method.ok_or)<E>(self, err: E) -> [Result](../result/enum.Result.html)<T, E>

Transforms the `Option<T>` into a [`Result<T, E>`](../result/enum.Result.html), mapping [`Some(v)`](enum.Option.html#variant.Some) to [`Ok(v)`](../result/enum.Result.html#variant.Ok) and [`None`](enum.Option.html#variant.None) to [`Err(err)`](../result/enum.Result.html#variant.Err).

Arguments passed to `ok_or` are eagerly evaluated; if you are passing the result of a function call, it is recommended to use [`ok_or_else`](enum.Option.html#method.ok_or_else), which is lazily evaluated.

##### [§](#examples-18)Examples

```
let x = Some("foo");
assert_eq!(x.ok_or(0), Ok("foo"));

let x: Option<&str> = None;
assert_eq!(x.ok_or(0), Err(0));
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1359-1361)

#### pub fn [ok_or_else](#method.ok_or_else)<E, F>(self, err: F) -> [Result](../result/enum.Result.html)<T, E>

where F: [FnOnce](../ops/trait.FnOnce.html)() -> E,

Transforms the `Option<T>` into a [`Result<T, E>`](../result/enum.Result.html), mapping [`Some(v)`](enum.Option.html#variant.Some) to [`Ok(v)`](../result/enum.Result.html#variant.Ok) and [`None`](enum.Option.html#variant.None) to [`Err(err())`](../result/enum.Result.html#variant.Err).

##### [§](#examples-19)Examples

```
let x = Some("foo");
assert_eq!(x.ok_or_else(|| 0), Ok("foo"));

let x: Option<&str> = None;
assert_eq!(x.ok_or_else(|| 0), Err(0));
```

1.40.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143773)) · [Source](../../src/core/option.rs.html#1386-1388)

#### pub fn [as_deref](#method.as_deref)(&self) -> [Option](enum.Option.html)<&<T as [Deref](../ops/trait.Deref.html)>::[Target](../ops/trait.Deref.html#associatedtype.Target)>

where T: [Deref](../ops/trait.Deref.html),

Converts from `Option<T>` (or `&Option<T>`) to `Option<&T::Target>`.

Leaves the original Option in-place, creating a new one with a reference to the original one, additionally coercing the contents via [`Deref`](../ops/trait.Deref.html).

##### [§](#examples-20)Examples

```
let x: Option<String> = Some("hey".to_owned());
assert_eq!(x.as_deref(), Some("hey"));

let x: Option<String> = None;
assert_eq!(x.as_deref(), None);
```

1.40.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143773)) · [Source](../../src/core/option.rs.html#1410-1412)

#### pub fn [as_deref_mut](#method.as_deref_mut)(&mut self) -> [Option](enum.Option.html)<&mut <T as [Deref](../ops/trait.Deref.html)>::[Target](../ops/trait.Deref.html#associatedtype.Target)>

where T: [DerefMut](../ops/trait.DerefMut.html),

Converts from `Option<T>` (or `&mut Option<T>`) to `Option<&mut T::Target>`.

Leaves the original `Option` in-place, creating a new one containing a mutable reference to the inner type’s [`Deref::Target`](../ops/trait.Deref.html#associatedtype.Target) type.

##### [§](#examples-21)Examples

```
let mut x: Option<String> = Some("hey".to_owned());
assert_eq!(x.as_deref_mut().map(|x| {
    x.make_ascii_uppercase();
    x
}), Some("HEY".to_owned().as_mut_str()));
```

1.0.0 · [Source](../../src/core/option.rs.html#1434)

#### pub fn [iter](#method.iter)(&self) -> [Iter](struct.Iter.html)<'_, T> [ⓘ](#)

Returns an iterator over the possibly contained value.

##### [§](#examples-22)Examples

```
let x = Some(4);
assert_eq!(x.iter().next(), Some(&4));

let x: Option<u32> = None;
assert_eq!(x.iter().next(), None);
```

1.0.0 · [Source](../../src/core/option.rs.html#1455)

#### pub fn [iter_mut](#method.iter_mut)(&mut self) -> [IterMut](struct.IterMut.html)<'_, T> [ⓘ](#)

Returns a mutable iterator over the possibly contained value.

##### [§](#examples-23)Examples

```
let mut x = Some(4);
match x.iter_mut().next() {
    Some(v) => *v = 42,
    None => {},
}
assert_eq!(x, Some(42));

let mut x: Option<u32> = None;
assert_eq!(x.iter_mut().next(), None);
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1493-1496)

#### pub fn [and](#method.and)<U>(self, optb: [Option](enum.Option.html)<U>) -> [Option](enum.Option.html)<U>

Returns [`None`](enum.Option.html#variant.None) if the option is [`None`](enum.Option.html#variant.None), otherwise returns `optb`.

Arguments passed to `and` are eagerly evaluated; if you are passing the result of a function call, it is recommended to use [`and_then`](enum.Option.html#method.and_then), which is lazily evaluated.

##### [§](#examples-24)Examples

```
let x = Some(2);
let y: Option<&str> = None;
assert_eq!(x.and(y), None);

let x: Option<u32> = None;
let y = Some("foo");
assert_eq!(x.and(y), None);

let x = Some(2);
let y = Some("foo");
assert_eq!(x.and(y), Some("foo"));

let x: Option<u32> = None;
let y: Option<&str> = None;
assert_eq!(x.and(y), None);
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1537-1539)

#### pub fn [and_then](#method.and_then)<U, F>(self, f: F) -> [Option](enum.Option.html)<U>

where F: [FnOnce](../ops/trait.FnOnce.html)(T) -> [Option](enum.Option.html)<U>,

Returns [`None`](enum.Option.html#variant.None) if the option is [`None`](enum.Option.html#variant.None), otherwise calls `f` with the wrapped value and returns the result.

Some languages call this operation flatmap.

##### [§](#examples-25)Examples

```
fn sq_then_to_string(x: u32) -> Option<String> {
    x.checked_mul(x).map(|sq| sq.to_string())
}

assert_eq!(Some(2).and_then(sq_then_to_string), Some(4.to_string()));
assert_eq!(Some(1_000_000).and_then(sq_then_to_string), None); // overflowed!
assert_eq!(None.and_then(sq_then_to_string), None);
```

Often used to chain fallible operations that may return [`None`](enum.Option.html#variant.None).

```
let arr_2d = [["A0", "A1"], ["B0", "B1"]];

let item_0_1 = arr_2d.get(0).and_then(|row| row.get(1));
assert_eq!(item_0_1, Some(&"A1"));

let item_2_0 = arr_2d.get(2).and_then(|row| row.get(0));
assert_eq!(item_2_0, None);
```

1.27.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1574-1577)

#### pub fn [filter](#method.filter)<P>(self, predicate: P) -> [Option](enum.Option.html)<T>

where P: [FnOnce](../ops/trait.FnOnce.html)([&T](../primitive.reference.html)) -> [bool](../primitive.bool.html),

Returns [`None`](enum.Option.html#variant.None) if the option is [`None`](enum.Option.html#variant.None), otherwise calls `predicate` with the wrapped value and returns:

- [`Some(t)`](enum.Option.html#variant.Some) if `predicate` returns `true` (where `t` is the wrapped value), and
- [`None`](enum.Option.html#variant.None) if `predicate` returns `false`.

This function works similar to [`Iterator::filter()`](../iter/trait.Iterator.html#method.filter). You can imagine the `Option<T>` being an iterator over one or zero elements. `filter()` lets you decide which elements to keep.

##### [§](#examples-26)Examples

```
fn is_even(n: &i32) -> bool {
    n % 2 == 0
}

assert_eq!(None.filter(is_even), None);
assert_eq!(Some(3).filter(is_even), None);
assert_eq!(Some(4).filter(is_even), Some(4));
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1617-1619)

#### pub fn [or](#method.or)(self, optb: [Option](enum.Option.html)<T>) -> [Option](enum.Option.html)<T>

Returns the option if it contains a value, otherwise returns `optb`.

Arguments passed to `or` are eagerly evaluated; if you are passing the result of a function call, it is recommended to use [`or_else`](enum.Option.html#method.or_else), which is lazily evaluated.

##### [§](#examples-27)Examples

```
let x = Some(2);
let y = None;
assert_eq!(x.or(y), Some(2));

let x = None;
let y = Some(100);
assert_eq!(x.or(y), Some(100));

let x = Some(2);
let y = Some(100);
assert_eq!(x.or(y), Some(2));

let x: Option<u32> = None;
let y = None;
assert_eq!(x.or(y), None);
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1643-1648)

#### pub fn [or_else](#method.or_else)<F>(self, f: F) -> [Option](enum.Option.html)<T>

where F: [FnOnce](../ops/trait.FnOnce.html)() -> [Option](enum.Option.html)<T>,

Returns the option if it contains a value, otherwise calls `f` and returns the result.

##### [§](#examples-28)Examples

```
fn nobody() -> Option<&'static str> { None }
fn vikings() -> Option<&'static str> { Some("vikings") }

assert_eq!(Some("barbarians").or_else(vikings), Some("barbarians"));
assert_eq!(None.or_else(vikings), Some("vikings"));
assert_eq!(None.or_else(nobody), None);
```

1.37.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1680-1682)

#### pub fn [xor](#method.xor)(self, optb: [Option](enum.Option.html)<T>) -> [Option](enum.Option.html)<T>

Returns [`Some`](enum.Option.html#variant.Some) if exactly one of `self`, `optb` is [`Some`](enum.Option.html#variant.Some), otherwise returns [`None`](enum.Option.html#variant.None).

##### [§](#examples-29)Examples

```
let x = Some(2);
let y: Option<u32> = None;
assert_eq!(x.xor(y), Some(2));

let x: Option<u32> = None;
let y = Some(2);
assert_eq!(x.xor(y), Some(2));

let x = Some(2);
let y = Some(2);
assert_eq!(x.xor(y), None);

let x: Option<u32> = None;
let y: Option<u32> = None;
assert_eq!(x.xor(y), None);
```

1.53.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1718-1720)

#### pub fn [insert](#method.insert)(&mut self, value: T) -> [&mut T](../primitive.reference.html)

Inserts `value` into the option, then returns a mutable reference to it.

If the option already contains a value, the old value is dropped.

See also [`Option::get_or_insert`](enum.Option.html#method.get_or_insert), which doesn’t update the value if the option already contains [`Some`](enum.Option.html#variant.Some).

##### [§](#example)Example

```
let mut opt = None;
let val = opt.insert(1);
assert_eq!(*val, 1);
assert_eq!(opt.unwrap(), 1);
let val = opt.insert(2);
assert_eq!(*val, 2);
*val = 3;
assert_eq!(opt.unwrap(), 3);
```

1.20.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1751-1753)

#### pub fn [get_or_insert](#method.get_or_insert)(&mut self, value: T) -> [&mut T](../primitive.reference.html)

Inserts `value` into the option if it is [`None`](enum.Option.html#variant.None), then returns a mutable reference to the contained value.

See also [`Option::insert`](enum.Option.html#method.insert), which updates the value even if the option already contains [`Some`](enum.Option.html#variant.Some).

##### [§](#examples-30)Examples

```
let mut x = None;

{
    let y: &mut u32 = x.get_or_insert(5);
    assert_eq!(y, &5);

    *y = 7;
}

assert_eq!(x, Some(7));
```

1.83.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1778-1780)

#### pub fn [get_or_insert_default](#method.get_or_insert_default)(&mut self) -> [&mut T](../primitive.reference.html)

where T: [Default](../default/trait.Default.html),

Inserts the default value into the option if it is [`None`](enum.Option.html#variant.None), then returns a mutable reference to the contained value.

##### [§](#examples-31)Examples

```
let mut x = None;

{
    let y: &mut u32 = x.get_or_insert_default();
    assert_eq!(y, &0);

    *y = 7;
}

assert_eq!(x, Some(7));
```

1.20.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1805-1807)

#### pub fn [get_or_insert_with](#method.get_or_insert_with)<F>(&mut self, f: F) -> [&mut T](../primitive.reference.html)

where F: [FnOnce](../ops/trait.FnOnce.html)() -> T,

Inserts a value computed from `f` into the option if it is [`None`](enum.Option.html#variant.None), then returns a mutable reference to the contained value.

##### [§](#examples-32)Examples

```
let mut x = None;

{
    let y: &mut u32 = x.get_or_insert_with(|| 5);
    assert_eq!(y, &5);

    *y = 7;
}

assert_eq!(x, Some(7));
```

[Source](../../src/core/option.rs.html#1860-1866)

#### pub fn [get_or_try_insert_with](#method.get_or_try_insert_with)<'a, R, F>( &'a mut self, f: F, ) -> <<R as [Try](../ops/trait.Try.html)>::[Residual](../ops/trait.Try.html#associatedtype.Residual) as [Residual](../ops/trait.Residual.html)<[&'a mut T](../primitive.reference.html)>>::[TryType](../ops/trait.Residual.html#associatedtype.TryType)

where F: [FnOnce](../ops/trait.FnOnce.html)() -> R, R: [Try](../ops/trait.Try.html)<Output = T>, <R as [Try](../ops/trait.Try.html)>::[Residual](../ops/trait.Try.html#associatedtype.Residual): [Residual](../ops/trait.Residual.html)<[&'a mut T](../primitive.reference.html)>,

🔬This is a nightly-only experimental API. (`option_get_or_try_insert_with` [#143648](https://github.com/rust-lang/rust/issues/143648))

If the option is `None`, calls the closure and inserts its output if successful.

If the closure returns a residual value such as `Err` or `None`, that residual value is returned and nothing is inserted.

If the option is `Some`, nothing is inserted.

Unless a residual is returned, a mutable reference to the value of the option will be output.

##### [§](#examples-33)Examples

```
#![feature(option_get_or_try_insert_with)]
let mut o1: Option<u32> = None;
let mut o2: Option<u8> = None;

let number = "12345";

assert_eq!(o1.get_or_try_insert_with(|| number.parse()).copied(), Ok(12345));
assert!(o2.get_or_try_insert_with(|| number.parse()).is_err());
assert_eq!(o1, Some(12345));
assert_eq!(o2, None);
```

1.0.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#1899)

#### pub const fn [take](#method.take)(&mut self) -> [Option](enum.Option.html)<T>

Takes the value out of the option, leaving a [`None`](enum.Option.html#variant.None) in its place.

##### [§](#examples-34)Examples

```
let mut x = Some(2);
let y = x.take();
assert_eq!(x, None);
assert_eq!(y, Some(2));

let mut x: Option<u32> = None;
let y = x.take();
assert_eq!(x, None);
assert_eq!(y, None);
```

1.80.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1931-1933)

#### pub fn [take_if](#method.take_if)<P>(&mut self, predicate: P) -> [Option](enum.Option.html)<T>

where P: [FnOnce](../ops/trait.FnOnce.html)([&mut T](../primitive.reference.html)) -> [bool](../primitive.bool.html),

Takes the value out of the option, but only if the predicate evaluates to `true` on a mutable reference to the value.

In other words, replaces `self` with `None` if the predicate returns `true`. This method operates similar to [`Option::take`](enum.Option.html#method.take) but conditional.

##### [§](#examples-35)Examples

```
let mut x = Some(42);

let prev = x.take_if(|v| if *v == 42 {
    *v += 1;
    false
} else {
    false
});
assert_eq!(x, Some(43));
assert_eq!(prev, None);

let prev = x.take_if(|v| *v == 43);
assert_eq!(x, None);
assert_eq!(prev, Some(43));
```

1.31.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#1958)

#### pub const fn [replace](#method.replace)(&mut self, value: T) -> [Option](enum.Option.html)<T>

Replaces the actual value in the option by the value given in parameter, returning the old value if present, leaving a [`Some`](enum.Option.html#variant.Some) in its place without deinitializing either one.

##### [§](#examples-36)Examples

```
let mut x = Some(2);
let old = x.replace(5);
assert_eq!(x, Some(5));
assert_eq!(old, Some(2));

let mut x = None;
let old = x.replace(3);
assert_eq!(x, Some(3));
assert_eq!(old, None);
```

1.46.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143956)) · [Source](../../src/core/option.rs.html#1979-1982)

#### pub fn [zip](#method.zip)<U>(self, other: [Option](enum.Option.html)<U>) -> [Option](enum.Option.html)<[(T, U)](../primitive.tuple.html)>

Makes a tuple of the value in `self` and the value in another `Option`.

If `self` is `Some(s)` and `other` is `Some(o)`, this method returns `Some((s, o))`. Otherwise, `None` is returned.

##### [§](#examples-37)Examples

```
let x = Some(1);
let y = Some("hi");
let z = None::<u8>;

assert_eq!(x.zip(y), Some((1, "hi")));
assert_eq!(x.zip(z), None);
```

[Source](../../src/core/option.rs.html#2020-2024)

#### pub const fn [zip_with](#method.zip_with)<U, F, R>(self, other: [Option](enum.Option.html)<U>, f: F) -> [Option](enum.Option.html)<R>

where F: [FnOnce](../ops/trait.FnOnce.html)(T, U) -> R,

🔬This is a nightly-only experimental API. (`option_zip` [#70086](https://github.com/rust-lang/rust/issues/70086))

Combines the value in `self` with the value in another `Option`, using the function `f`.

If `self` is `Some(s)` and `other` is `Some(o)`, this method returns `Some(f(s, o))`. Otherwise, `None` is returned.

##### [§](#examples-38)Examples

```
#![feature(option_zip)]

#[derive(Debug, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

let x = Some(17.5);
let y = Some(42.7);

assert_eq!(x.zip_with(y, Point::new), Some(Point { x: 17.5, y: 42.7 }));
assert_eq!(x.zip_with(None, Point::new), None);
```

[Source](../../src/core/option.rs.html#2054-2058)

#### pub fn [reduce](#method.reduce)<U, R, F>(self, other: [Option](enum.Option.html)<U>, f: F) -> [Option](enum.Option.html)<R>

where T: [Into](../convert/trait.Into.html)<R>, U: [Into](../convert/trait.Into.html)<R>, F: [FnOnce](../ops/trait.FnOnce.html)(T, U) -> R,

🔬This is a nightly-only experimental API. (`option_reduce` [#144273](https://github.com/rust-lang/rust/issues/144273))

Reduces two options into one, using the provided function if both are `Some`.

If `self` is `Some(s)` and `other` is `Some(o)`, this method returns `Some(f(s, o))`. Otherwise, if only one of `self` and `other` is `Some`, that one is returned. If both `self` and `other` are `None`, `None` is returned.

##### [§](#examples-39)Examples

```
#![feature(option_reduce)]

let s12 = Some(12);
let s17 = Some(17);
let n = None;
let f = |a, b| a + b;

assert_eq!(s12.reduce(s17, f), Some(29));
assert_eq!(s12.reduce(n, f), Some(12));
assert_eq!(n.reduce(s17, f), Some(17));
assert_eq!(n.reduce(n, f), None);
```

[Source](../../src/core/option.rs.html#2069)[§](#impl-Option%3CT%3E-1)

### impl<T> [Option](enum.Option.html)<T>

where T: [IntoIterator](../iter/trait.IntoIterator.html),

[Source](../../src/core/option.rs.html#2085)

#### pub fn [into_flat_iter](#method.into_flat_iter)(self) -> [OptionFlatten](struct.OptionFlatten.html)<<T as [IntoIterator](../iter/trait.IntoIterator.html)>::[IntoIter](../iter/trait.IntoIterator.html#associatedtype.IntoIter)> [ⓘ](#)

🔬This is a nightly-only experimental API. (`option_into_flat_iter` [#148441](https://github.com/rust-lang/rust/issues/148441))

Transforms an optional iterator into an iterator.

If `self` is `None`, the resulting iterator is empty. Otherwise, an iterator is made from the `Some` value and returned.

##### [§](#examples-40)Examples

```
#![feature(option_into_flat_iter)]

let o1 = Some([1, 2]);
let o2 = None::<&[usize]>;

assert_eq!(o1.into_flat_iter().collect::<Vec<_>>(), [1, 2]);
assert_eq!(o2.into_flat_iter().collect::<Vec<_>>(), Vec::<&usize>::new());
```

[Source](../../src/core/option.rs.html#2090)[§](#impl-Option%3C(T,+U)%3E)

### impl<T, U> [Option](enum.Option.html)<[(T, U)](../primitive.tuple.html)>

1.66.0 · [Source](../../src/core/option.rs.html#2107)

#### pub fn [unzip](#method.unzip)(self) -> ([Option](enum.Option.html)<T>, [Option](enum.Option.html)<U>)

Unzips an option containing a tuple of two options.

If `self` is `Some((a, b))` this method returns `(Some(a), Some(b))`. Otherwise, `(None, None)` is returned.

##### [§](#examples-41)Examples

```
let x = Some((1, "hi"));
let y = None::<(u8, u32)>;

assert_eq!(x.unzip(), (Some(1), Some("hi")));
assert_eq!(y.unzip(), (None, None));
```

[Source](../../src/core/option.rs.html#2115)[§](#impl-Option%3C%26T%3E)

### impl<T> [Option](enum.Option.html)<[&T](../primitive.reference.html)>

1.35.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#2131-2133)

#### pub const fn [copied](#method.copied)(self) -> [Option](enum.Option.html)<T>

where T: [Copy](../marker/trait.Copy.html),

Maps an `Option<&T>` to an `Option<T>` by copying the contents of the option.

##### [§](#examples-42)Examples

```
let x = 12;
let opt_x = Some(&x);
assert_eq!(opt_x, Some(&12));
let copied = opt_x.copied();
assert_eq!(copied, Some(12));
```

1.0.0 · [Source](../../src/core/option.rs.html#2157-2159)

#### pub fn [cloned](#method.cloned)(self) -> [Option](enum.Option.html)<T>

where T: [Clone](../clone/trait.Clone.html),

Maps an `Option<&T>` to an `Option<T>` by cloning the contents of the option.

##### [§](#examples-43)Examples

```
let x = 12;
let opt_x = Some(&x);
assert_eq!(opt_x, Some(&12));
let cloned = opt_x.cloned();
assert_eq!(cloned, Some(12));
```

[Source](../../src/core/option.rs.html#2165)[§](#impl-Option%3C%26mut+T%3E)

### impl<T> [Option](enum.Option.html)<[&mut T](../primitive.reference.html)>

1.35.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#2181-2183)

#### pub const fn [copied](#method.copied-1)(self) -> [Option](enum.Option.html)<T>

where T: [Copy](../marker/trait.Copy.html),

Maps an `Option<&mut T>` to an `Option<T>` by copying the contents of the option.

##### [§](#examples-44)Examples

```
let mut x = 12;
let opt_x = Some(&mut x);
assert_eq!(opt_x, Some(&mut 12));
let copied = opt_x.copied();
assert_eq!(copied, Some(12));
```

1.26.0 · [Source](../../src/core/option.rs.html#2205-2207)

#### pub fn [cloned](#method.cloned-1)(self) -> [Option](enum.Option.html)<T>

where T: [Clone](../clone/trait.Clone.html),

Maps an `Option<&mut T>` to an `Option<T>` by cloning the contents of the option.

##### [§](#examples-45)Examples

```
let mut x = 12;
let opt_x = Some(&mut x);
assert_eq!(opt_x, Some(&mut 12));
let cloned = opt_x.cloned();
assert_eq!(cloned, Some(12));
```

[Source](../../src/core/option.rs.html#2213)[§](#impl-Option%3CResult%3CT,+E%3E%3E)

### impl<T, E> [Option](enum.Option.html)<[Result](../result/enum.Result.html)<T, E>>

1.33.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#2234)

#### pub const fn [transpose](#method.transpose)(self) -> [Result](../result/enum.Result.html)<[Option](enum.Option.html)<T>, E>

Transposes an `Option` of a [`Result`](../result/enum.Result.html) into a [`Result`](../result/enum.Result.html) of an `Option`.

`[Some](enum.Option.html#variant.Some)([Ok](../result/enum.Result.html#variant.Ok)(_))` is mapped to `[Ok](../result/enum.Result.html#variant.Ok)([Some](enum.Option.html#variant.Some)(_))`, `[Some](enum.Option.html#variant.Some)([Err](../result/enum.Result.html#variant.Err)(_))` is mapped to `[Err](../result/enum.Result.html#variant.Err)(_)`, and [`None`](enum.Option.html#variant.None) will be mapped to `[Ok](../result/enum.Result.html#variant.Ok)([None](enum.Option.html#variant.None))`.

##### [§](#examples-46)Examples

```
#[derive(Debug, Eq, PartialEq)]
struct SomeErr;

let x: Option<Result<i32, SomeErr>> = Some(Ok(5));
let y: Result<Option<i32>, SomeErr> = Ok(Some(5));
assert_eq!(x.transpose(), y);
```

[Source](../../src/core/option.rs.html#2905)[§](#impl-Option%3COption%3CT%3E%3E)

### impl<T> [Option](enum.Option.html)<[Option](enum.Option.html)<T>>

1.40.0 (const: 1.83.0) · [Source](../../src/core/option.rs.html#2934)

#### pub const fn [flatten](#method.flatten)(self) -> [Option](enum.Option.html)<T>

Converts from `Option<Option<T>>` to `Option<T>`.

##### [§](#examples-47)Examples

Basic usage:

```
let x: Option<Option<u32>> = Some(Some(6));
assert_eq!(Some(6), x.flatten());

let x: Option<Option<u32>> = Some(None);
assert_eq!(None, x.flatten());

let x: Option<Option<u32>> = None;
assert_eq!(None, x.flatten());
```

Flattening only removes one level of nesting at a time:

```
let x: Option<Option<Option<u32>>> = Some(Some(Some(6)));
assert_eq!(Some(Some(6)), x.flatten());
assert_eq!(Some(6), x.flatten().flatten());
```

[Source](../../src/core/option.rs.html#2943)[§](#impl-Option%3C%26Option%3CT%3E%3E)

### impl<'a, T> [Option](enum.Option.html)<&'a [Option](enum.Option.html)<T>>

[Source](../../src/core/option.rs.html#2964)

#### pub const fn [flatten_ref](#method.flatten_ref)(self) -> [Option](enum.Option.html)<[&'a T](../primitive.reference.html)>

🔬This is a nightly-only experimental API. (`option_reference_flattening` [#149221](https://github.com/rust-lang/rust/issues/149221))

Converts from `Option<&Option<T>>` to `Option<&T>`.

##### [§](#examples-48)Examples

Basic usage:

```
#![feature(option_reference_flattening)]

let x: Option<&Option<u32>> = Some(&Some(6));
assert_eq!(Some(&6), x.flatten_ref());

let x: Option<&Option<u32>> = Some(&None);
assert_eq!(None, x.flatten_ref());

let x: Option<&Option<u32>> = None;
assert_eq!(None, x.flatten_ref());
```

[Source](../../src/core/option.rs.html#2972)[§](#impl-Option%3C%26mut+Option%3CT%3E%3E)

### impl<'a, T> [Option](enum.Option.html)<&'a mut [Option](enum.Option.html)<T>>

[Source](../../src/core/option.rs.html#2995)

#### pub const fn [flatten_ref](#method.flatten_ref-1)(self) -> [Option](enum.Option.html)<[&'a T](../primitive.reference.html)>

🔬This is a nightly-only experimental API. (`option_reference_flattening` [#149221](https://github.com/rust-lang/rust/issues/149221))

Converts from `Option<&mut Option<T>>` to `&Option<T>`.

##### [§](#examples-49)Examples

Basic usage:

```
#![feature(option_reference_flattening)]

let y = &mut Some(6);
let x: Option<&mut Option<u32>> = Some(y);
assert_eq!(Some(&6), x.flatten_ref());

let y: &mut Option<u32> = &mut None;
let x: Option<&mut Option<u32>> = Some(y);
assert_eq!(None, x.flatten_ref());

let x: Option<&mut Option<u32>> = None;
assert_eq!(None, x.flatten_ref());
```

[Source](../../src/core/option.rs.html#3024)

#### pub const fn [flatten_mut](#method.flatten_mut)(self) -> [Option](enum.Option.html)<[&'a mut T](../primitive.reference.html)>

🔬This is a nightly-only experimental API. (`option_reference_flattening` [#149221](https://github.com/rust-lang/rust/issues/149221))

Converts from `Option<&mut Option<T>>` to `Option<&mut T>`.

##### [§](#examples-50)Examples

Basic usage:

```
#![feature(option_reference_flattening)]

let y: &mut Option<u32> = &mut Some(6);
let x: Option<&mut Option<u32>> = Some(y);
assert_eq!(Some(&mut 6), x.flatten_mut());

let y: &mut Option<u32> = &mut None;
let x: Option<&mut Option<u32>> = Some(y);
assert_eq!(None, x.flatten_mut());

let x: Option<&mut Option<u32>> = None;
assert_eq!(None, x.flatten_mut());
```

## Trait Implementations[§](#trait-implementations)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/142757)) · [Source](../../src/core/option.rs.html#2266-2270)[§](#impl-Clone-for-Option%3CT%3E)

### impl<T> [Clone](../clone/trait.Clone.html) for [Option](enum.Option.html)<T>

where T: [Clone](../clone/trait.Clone.html),

[Source](../../src/core/option.rs.html#2273)[§](#method.clone)

#### fn [clone](../clone/trait.Clone.html#tymethod.clone)(&self) -> [Option](enum.Option.html)<T>

Returns a duplicate of the value. [Read more](../clone/trait.Clone.html#tymethod.clone)

[Source](../../src/core/option.rs.html#2281)[§](#method.clone_from)

#### fn [clone_from](../clone/trait.Clone.html#method.clone_from)(&mut self, source: &[Option](enum.Option.html)<T>)

Performs copy-assignment from `source`. [Read more](../clone/trait.Clone.html#method.clone_from)

[Source](../../src/core/cell.rs.html#809)[§](#impl-CloneFromCell-for-Option%3CT%3E)

### impl<T> [CloneFromCell](../cell/trait.CloneFromCell.html) for [Option](enum.Option.html)<T>

where T: [CloneFromCell](../cell/trait.CloneFromCell.html),

1.0.0 · [Source](../../src/core/option.rs.html#592)[§](#impl-Copy-for-Option%3CT%3E)

### impl<T> [Copy](../marker/trait.Copy.html) for [Option](enum.Option.html)<T>

where T: [Copy](../marker/trait.Copy.html),

1.0.0 · [Source](../../src/core/option.rs.html#592)[§](#impl-Debug-for-Option%3CT%3E)

### impl<T> [Debug](../fmt/trait.Debug.html) for [Option](enum.Option.html)<T>

where T: [Debug](../fmt/trait.Debug.html),

[Source](../../src/core/option.rs.html#592)[§](#method.fmt)

#### fn [fmt](../fmt/trait.Debug.html#tymethod.fmt)(&self, f: &mut [Formatter](../fmt/struct.Formatter.html)<'_>) -> [Result](../result/enum.Result.html)<[()](../primitive.unit.html), [Error](../fmt/struct.Error.html)>

Formats the value using the given formatter. [Read more](../fmt/trait.Debug.html#tymethod.fmt)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143894)) · [Source](../../src/core/option.rs.html#2299)[§](#impl-Default-for-Option%3CT%3E)

### impl<T> [Default](../default/trait.Default.html) for [Option](enum.Option.html)<T>

[Source](../../src/core/option.rs.html#2309)[§](#method.default)

#### fn [default](../default/trait.Default.html#tymethod.default)() -> [Option](enum.Option.html)<T>

Returns [`None`](enum.Option.html#variant.None).

##### [§](#examples-51)Examples

```
let opt: Option<u32> = Option::default();
assert!(opt.is_none());
```

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/118304)) · [Source](../../src/core/option.rs.html#593)[§](#impl-Eq-for-Option%3CT%3E)

### impl<T> [Eq](../cmp/trait.Eq.html) for [Option](enum.Option.html)<T>

where T: [Eq](../cmp/trait.Eq.html),

1.30.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143773)) · [Source](../../src/core/option.rs.html#2378)[§](#impl-From%3C%26Option%3CT%3E%3E-for-Option%3C%26T%3E)

### impl<'a, T> [From](../convert/trait.From.html)<&'a [Option](enum.Option.html)<T>> for [Option](enum.Option.html)<[&'a T](../primitive.reference.html)>

[Source](../../src/core/option.rs.html#2399)[§](#method.from)

#### fn [from](../convert/trait.From.html#tymethod.from)(o: &'a [Option](enum.Option.html)<T>) -> [Option](enum.Option.html)<[&'a T](../primitive.reference.html)>

Converts from `&Option<T>` to `Option<&T>`.

##### [§](#examples-52)Examples

Converts an `[Option](enum.Option.html)<[String](../../std/string/struct.String.html)>` into an `[Option](enum.Option.html)<[usize](../primitive.usize.html)>`, preserving the original. The [`map`](enum.Option.html#method.map) method takes the `self` argument by value, consuming the original, so this technique uses `from` to first take an [`Option`](enum.Option.html) to a reference to the value inside the original.

```
let s: Option<String> = Some(String::from("Hello, Rustaceans!"));
let o: Option<usize> = Option::from(&s).map(|ss: &String| ss.len());

println!("Can still print s: {s:?}");

assert_eq!(o, Some(18));
```

1.30.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143773)) · [Source](../../src/core/option.rs.html#2406)[§](#impl-From%3C%26mut+Option%3CT%3E%3E-for-Option%3C%26mut+T%3E)

### impl<'a, T> [From](../convert/trait.From.html)<&'a mut [Option](enum.Option.html)<T>> for [Option](enum.Option.html)<[&'a mut T](../primitive.reference.html)>

[Source](../../src/core/option.rs.html#2422)[§](#method.from-1)

#### fn [from](../convert/trait.From.html#tymethod.from)(o: &'a mut [Option](enum.Option.html)<T>) -> [Option](enum.Option.html)<[&'a mut T](../primitive.reference.html)>

Converts from `&mut Option<T>` to `Option<&mut T>`

##### [§](#examples-53)Examples

```
let mut s = Some(String::from("Hello"));
let o: Option<&mut String> = Option::from(&mut s);

match o {
    Some(t) => *t = String::from("Hello, Rustaceans!"),
    None => (),
}

assert_eq!(s, Some(String::from("Hello, Rustaceans!")));
```

1.12.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143773)) · [Source](../../src/core/option.rs.html#2361)[§](#impl-From%3CT%3E-for-Option%3CT%3E)

### impl<T> [From](../convert/trait.From.html)<T> for [Option](enum.Option.html)<T>

[Source](../../src/core/option.rs.html#2371)[§](#method.from-2)

#### fn [from](../convert/trait.From.html#tymethod.from)(val: T) -> [Option](enum.Option.html)<T>

Moves `val` into a new [`Some`](enum.Option.html#variant.Some).

##### [§](#examples-54)Examples

```
let o: Option<u8> = Option::from(67);

assert_eq!(Some(67), o);
```

1.0.0 · [Source](../../src/core/option.rs.html#2789)[§](#impl-FromIterator%3COption%3CA%3E%3E-for-Option%3CV%3E)

### impl<A, V> [FromIterator](../iter/trait.FromIterator.html)<[Option](enum.Option.html)<A>> for [Option](enum.Option.html)<V>

where V: [FromIterator](../iter/trait.FromIterator.html)<A>,

[Source](../../src/core/option.rs.html#2851)[§](#method.from_iter)

#### fn [from_iter](../iter/trait.FromIterator.html#tymethod.from_iter)<I>(iter: I) -> [Option](enum.Option.html)<V>

where I: [IntoIterator](../iter/trait.IntoIterator.html)<Item = [Option](enum.Option.html)<A>>,

Takes each element in the [`Iterator`](../iter/trait.Iterator.html): if it is [`None`](enum.Option.html#variant.None), no further elements are taken, and the [`None`](enum.Option.html#variant.None) is returned. Should no [`None`](enum.Option.html#variant.None) occur, a container of type `V` containing the values of each [`Option`](enum.Option.html) is returned.

##### [§](#examples-55)Examples

Here is an example which increments every integer in a vector. We use the checked variant of `add` that returns `None` when the calculation would result in an overflow.

```
let items = vec![0_u16, 1, 2];

let res: Option<Vec<u16>> = items
    .iter()
    .map(|x| x.checked_add(1))
    .collect();

assert_eq!(res, Some(vec![1, 2, 3]));
```

As you can see, this will return the expected, valid items.

Here is another example that tries to subtract one from another list of integers, this time checking for underflow:

```
let items = vec![2_u16, 1, 0];

let res: Option<Vec<u16>> = items
    .iter()
    .map(|x| x.checked_sub(1))
    .collect();

assert_eq!(res, None);
```

Since the last element is zero, it would underflow. Thus, the resulting value is `None`.

Here is a variation on the previous example, showing that no further elements are taken from `iter` after the first `None`.

```
let items = vec![3_u16, 2, 1, 10];

let mut shared = 0;

let res: Option<Vec<u16>> = items
    .iter()
    .map(|x| { shared += x; x.checked_sub(2) })
    .collect();

assert_eq!(res, None);
assert_eq!(shared, 6);
```

Since the third element caused an underflow, no further elements were taken, so the final value of `shared` is 6 (= `3 + 2 + 1`), not 16.

[Source](../../src/core/option.rs.html#2880)[§](#impl-FromResidual%3COption%3CInfallible%3E%3E-for-Option%3CT%3E)

### impl<T> [FromResidual](../ops/trait.FromResidual.html)<[Option](enum.Option.html)<[Infallible](../convert/enum.Infallible.html)>> for [Option](enum.Option.html)<T>

[Source](../../src/core/option.rs.html#2882)[§](#method.from_residual)

#### fn [from_residual](../ops/trait.FromResidual.html#tymethod.from_residual)(residual: [Option](enum.Option.html)<[Infallible](../convert/enum.Infallible.html)>) -> [Option](enum.Option.html)<T>

🔬This is a nightly-only experimental API. (`try_trait_v2` [#84277](https://github.com/rust-lang/rust/issues/84277))

Constructs the type from a compatible `Residual` type. [Read more](../ops/trait.FromResidual.html#tymethod.from_residual)

[Source](../../src/core/option.rs.html#2892)[§](#impl-FromResidual%3CYeet%3C()%3E%3E-for-Option%3CT%3E)

### impl<T> [FromResidual](../ops/trait.FromResidual.html)<[Yeet](../ops/struct.Yeet.html)<[()](../primitive.unit.html)>> for [Option](enum.Option.html)<T>

[Source](../../src/core/option.rs.html#2894)[§](#method.from_residual-1)

#### fn [from_residual](../ops/trait.FromResidual.html#tymethod.from_residual)(_: [Yeet](../ops/struct.Yeet.html)<[()](../primitive.unit.html)>) -> [Option](enum.Option.html)<T>

🔬This is a nightly-only experimental API. (`try_trait_v2` [#84277](https://github.com/rust-lang/rust/issues/84277))

Constructs the type from a compatible `Residual` type. [Read more](../ops/trait.FromResidual.html#tymethod.from_residual)

1.0.0 · [Source](../../src/core/option.rs.html#592)[§](#impl-Hash-for-Option%3CT%3E)

### impl<T> [Hash](../hash/trait.Hash.html) for [Option](enum.Option.html)<T>

where T: [Hash](../hash/trait.Hash.html),

[Source](../../src/core/option.rs.html#592)[§](#method.hash)

#### fn [hash](../hash/trait.Hash.html#tymethod.hash)<__H>(&self, state: [&mut __H](../primitive.reference.html))

where __H: [Hasher](../hash/trait.Hasher.html),

Feeds this value into the given [`Hasher`](../hash/trait.Hasher.html). [Read more](../hash/trait.Hash.html#tymethod.hash)

1.3.0 · [Source](../../src/core/hash/mod.rs.html#234-236)[§](#method.hash_slice)

#### fn [hash_slice](../hash/trait.Hash.html#method.hash_slice)<H>(data: &[Self], state: [&mut H](../primitive.reference.html))

where H: [Hasher](../hash/trait.Hasher.html), Self: [Sized](../marker/trait.Sized.html),

Feeds a slice of this type into the given [`Hasher`](../hash/trait.Hasher.html). [Read more](../hash/trait.Hash.html#method.hash_slice)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/92476)) · [Source](../../src/core/option.rs.html#2316)[§](#impl-IntoIterator-for-Option%3CT%3E)

### impl<T> [IntoIterator](../iter/trait.IntoIterator.html) for [Option](enum.Option.html)<T>

[Source](../../src/core/option.rs.html#2334)[§](#method.into_iter)

#### fn [into_iter](../iter/trait.IntoIterator.html#tymethod.into_iter)(self) -> [IntoIter](struct.IntoIter.html)<T> [ⓘ](#)

Returns a consuming iterator over the possibly contained value.

##### [§](#examples-56)Examples

```
let x = Some("string");
let v: Vec<&str> = x.into_iter().collect();
assert_eq!(v, ["string"]);

let x = None;
let v: Vec<&str> = x.into_iter().collect();
assert!(v.is_empty());
```

[Source](../../src/core/option.rs.html#2317)[§](#associatedtype.Item)

#### type [Item](../iter/trait.IntoIterator.html#associatedtype.Item) = T

The type of the elements being iterated over.

[Source](../../src/core/option.rs.html#2318)[§](#associatedtype.IntoIter)

#### type [IntoIter](../iter/trait.IntoIterator.html#associatedtype.IntoIter) = [IntoIter](struct.IntoIter.html)<T>

Which kind of iterator are we turning this into?

1.4.0 · [Source](../../src/core/option.rs.html#2340)[§](#impl-IntoIterator-for-%26Option%3CT%3E)

### impl<'a, T> [IntoIterator](../iter/trait.IntoIterator.html) for &'a [Option](enum.Option.html)<T>

[Source](../../src/core/option.rs.html#2341)[§](#associatedtype.Item-1)

#### type [Item](../iter/trait.IntoIterator.html#associatedtype.Item) = [&'a T](../primitive.reference.html)

The type of the elements being iterated over.

[Source](../../src/core/option.rs.html#2342)[§](#associatedtype.IntoIter-1)

#### type [IntoIter](../iter/trait.IntoIterator.html#associatedtype.IntoIter) = [Iter](struct.Iter.html)<'a, T>

Which kind of iterator are we turning this into?

[Source](../../src/core/option.rs.html#2344)[§](#method.into_iter-1)

#### fn [into_iter](../iter/trait.IntoIterator.html#tymethod.into_iter)(self) -> [Iter](struct.Iter.html)<'a, T> [ⓘ](#)

Creates an iterator from a value. [Read more](../iter/trait.IntoIterator.html#tymethod.into_iter)

1.4.0 · [Source](../../src/core/option.rs.html#2350)[§](#impl-IntoIterator-for-%26mut+Option%3CT%3E)

### impl<'a, T> [IntoIterator](../iter/trait.IntoIterator.html) for &'a mut [Option](enum.Option.html)<T>

[Source](../../src/core/option.rs.html#2351)[§](#associatedtype.Item-2)

#### type [Item](../iter/trait.IntoIterator.html#associatedtype.Item) = [&'a mut T](../primitive.reference.html)

The type of the elements being iterated over.

[Source](../../src/core/option.rs.html#2352)[§](#associatedtype.IntoIter-2)

#### type [IntoIter](../iter/trait.IntoIterator.html#associatedtype.IntoIter) = [IterMut](struct.IterMut.html)<'a, T>

Which kind of iterator are we turning this into?

[Source](../../src/core/option.rs.html#2354)[§](#method.into_iter-2)

#### fn [into_iter](../iter/trait.IntoIterator.html#tymethod.into_iter)(self) -> [IterMut](struct.IterMut.html)<'a, T> [ⓘ](#)

Creates an iterator from a value. [Read more](../iter/trait.IntoIterator.html#tymethod.into_iter)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/option.rs.html#2467)[§](#impl-Ord-for-Option%3CT%3E)

### impl<T> [Ord](../cmp/trait.Ord.html) for [Option](enum.Option.html)<T>

where T: [Ord](../cmp/trait.Ord.html),

[Source](../../src/core/option.rs.html#2469)[§](#method.cmp)

#### fn [cmp](../cmp/trait.Ord.html#tymethod.cmp)(&self, other: &[Option](enum.Option.html)<T>) -> [Ordering](../cmp/enum.Ordering.html)

This method returns an [`Ordering`](../cmp/enum.Ordering.html) between `self` and `other`. [Read more](../cmp/trait.Ord.html#tymethod.cmp)

1.21.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/cmp.rs.html#1046-1048)[§](#method.max)

#### fn [max](../cmp/trait.Ord.html#method.max)(self, other: Self) -> Self

where Self: [Sized](../marker/trait.Sized.html),

Compares and returns the maximum of two values. [Read more](../cmp/trait.Ord.html#method.max)

1.21.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/cmp.rs.html#1085-1087)[§](#method.min)

#### fn [min](../cmp/trait.Ord.html#method.min)(self, other: Self) -> Self

where Self: [Sized](../marker/trait.Sized.html),

Compares and returns the minimum of two values. [Read more](../cmp/trait.Ord.html#method.min)

1.50.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/cmp.rs.html#1111-1113)[§](#method.clamp)

#### fn [clamp](../cmp/trait.Ord.html#method.clamp)(self, min: Self, max: Self) -> Self

where Self: [Sized](../marker/trait.Sized.html),

Restrict a value to a certain interval. [Read more](../cmp/trait.Ord.html#method.clamp)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/option.rs.html#2434)[§](#impl-PartialEq-for-Option%3CT%3E)

### impl<T> [PartialEq](../cmp/trait.PartialEq.html) for [Option](enum.Option.html)<T>

where T: [PartialEq](../cmp/trait.PartialEq.html),

[Source](../../src/core/option.rs.html#2436)[§](#method.eq)

#### fn [eq](../cmp/trait.PartialEq.html#tymethod.eq)(&self, other: &[Option](enum.Option.html)<T>) -> [bool](../primitive.bool.html)

Equality operator `==`. [Read more](../cmp/trait.PartialEq.html#tymethod.eq)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/cmp.rs.html#275)[§](#method.ne)

#### fn [ne](../cmp/trait.PartialEq.html#method.ne)(&self, other: [&Rhs](../primitive.reference.html)) -> [bool](../primitive.bool.html)

Inequality operator `!=`. [Read more](../cmp/trait.PartialEq.html#method.ne)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/option.rs.html#2453)[§](#impl-PartialOrd-for-Option%3CT%3E)

### impl<T> [PartialOrd](../cmp/trait.PartialOrd.html) for [Option](enum.Option.html)<T>

where T: [PartialOrd](../cmp/trait.PartialOrd.html),

[Source](../../src/core/option.rs.html#2455)[§](#method.partial_cmp)

#### fn [partial_cmp](../cmp/trait.PartialOrd.html#tymethod.partial_cmp)(&self, other: &[Option](enum.Option.html)<T>) -> [Option](enum.Option.html)<[Ordering](../cmp/enum.Ordering.html)>

This method returns an ordering between `self` and `other` values if one exists. [Read more](../cmp/trait.PartialOrd.html#tymethod.partial_cmp)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/cmp.rs.html#1422)[§](#method.lt)

#### fn [lt](../cmp/trait.PartialOrd.html#method.lt)(&self, other: [&Rhs](../primitive.reference.html)) -> [bool](../primitive.bool.html)

Tests less than (for `self` and `other`) and is used by the `<` operator. [Read more](../cmp/trait.PartialOrd.html#method.lt)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/cmp.rs.html#1440)[§](#method.le)

#### fn [le](../cmp/trait.PartialOrd.html#method.le)(&self, other: [&Rhs](../primitive.reference.html)) -> [bool](../primitive.bool.html)

Tests less than or equal to (for `self` and `other`) and is used by the `<=` operator. [Read more](../cmp/trait.PartialOrd.html#method.le)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/cmp.rs.html#1458)[§](#method.gt)

#### fn [gt](../cmp/trait.PartialOrd.html#method.gt)(&self, other: [&Rhs](../primitive.reference.html)) -> [bool](../primitive.bool.html)

Tests greater than (for `self` and `other`) and is used by the `>` operator. [Read more](../cmp/trait.PartialOrd.html#method.gt)

1.0.0 (const: [unstable](https://github.com/rust-lang/rust/issues/143800)) · [Source](../../src/core/cmp.rs.html#1476)[§](#method.ge)

#### fn [ge](../cmp/trait.PartialOrd.html#method.ge)(&self, other: [&Rhs](../primitive.reference.html)) -> [bool](../primitive.bool.html)

Tests greater than or equal to (for `self` and `other`) and is used by the `>=` operator. [Read more](../cmp/trait.PartialOrd.html#method.ge)

1.37.0 · [Source](../../src/core/iter/traits/accum.rs.html#302-304)[§](#impl-Product%3COption%3CU%3E%3E-for-Option%3CT%3E)

### impl<T, U> [Product](../iter/trait.Product.html)<[Option](enum.Option.html)<U>> for [Option](enum.Option.html)<T>

where T: [Product](../iter/trait.Product.html)<U>,

[Source](../../src/core/iter/traits/accum.rs.html#323-325)[§](#method.product)

#### fn [product](../iter/trait.Product.html#tymethod.product)<I>(iter: I) -> [Option](enum.Option.html)<T>

where I: [Iterator](../iter/trait.Iterator.html)<Item = [Option](enum.Option.html)<U>>,

Takes each element in the [`Iterator`](../iter/trait.Iterator.html): if it is a [`None`](enum.Option.html#variant.None), no further elements are taken, and the [`None`](enum.Option.html#variant.None) is returned. Should no [`None`](enum.Option.html#variant.None) occur, the product of all elements is returned.

##### [§](#examples-57)Examples

This multiplies each number in a vector of strings, if a string could not be parsed the operation returns `None`:

```
let nums = vec!["5", "10", "1", "2"];
let total: Option<usize> = nums.iter().map(|w| w.parse::<usize>().ok()).product();
assert_eq!(total, Some(100));
let nums = vec!["5", "10", "one", "2"];
let total: Option<usize> = nums.iter().map(|w| w.parse::<usize>().ok()).product();
assert_eq!(total, None);
```

[Source](../../src/core/option.rs.html#2901)[§](#impl-Residual%3CT%3E-for-Option%3CInfallible%3E)

### impl<T> [Residual](../ops/trait.Residual.html)<T> for [Option](enum.Option.html)<[Infallible](../convert/enum.Infallible.html)>

[Source](../../src/core/option.rs.html#2902)[§](#associatedtype.TryType)

#### type [TryType](../ops/trait.Residual.html#associatedtype.TryType) = [Option](enum.Option.html)<T>

🔬This is a nightly-only experimental API. (`try_trait_v2_residual` [#91285](https://github.com/rust-lang/rust/issues/91285))

The “return” type of this meta-function.

1.0.0 · [Source](../../src/core/option.rs.html#2431)[§](#impl-StructuralPartialEq-for-Option%3CT%3E)

### impl<T> [StructuralPartialEq](../marker/trait.StructuralPartialEq.html) for [Option](enum.Option.html)<T>

1.37.0 · [Source](../../src/core/iter/traits/accum.rs.html#272-274)[§](#impl-Sum%3COption%3CU%3E%3E-for-Option%3CT%3E)

### impl<T, U> [Sum](../iter/trait.Sum.html)<[Option](enum.Option.html)<U>> for [Option](enum.Option.html)<T>

where T: [Sum](../iter/trait.Sum.html)<U>,

[Source](../../src/core/iter/traits/accum.rs.html#293-295)[§](#method.sum)

#### fn [sum](../iter/trait.Sum.html#tymethod.sum)<I>(iter: I) -> [Option](enum.Option.html)<T>

where I: [Iterator](../iter/trait.Iterator.html)<Item = [Option](enum.Option.html)<U>>,

Takes each element in the [`Iterator`](../iter/trait.Iterator.html): if it is a [`None`](enum.Option.html#variant.None), no further elements are taken, and the [`None`](enum.Option.html#variant.None) is returned. Should no [`None`](enum.Option.html#variant.None) occur, the sum of all elements is returned.

##### [§](#examples-58)Examples

This sums up the position of the character ‘a’ in a vector of strings, if a word did not have the character ‘a’ the operation returns `None`:

```
let words = vec!["have", "a", "great", "day"];
let total: Option<usize> = words.iter().map(|w| w.find('a')).sum();
assert_eq!(total, Some(5));
let words = vec!["have", "a", "good", "day"];
let total: Option<usize> = words.iter().map(|w| w.find('a')).sum();
assert_eq!(total, None);
```

[Source](../../src/core/option.rs.html#2858)[§](#impl-Try-for-Option%3CT%3E)

### impl<T> [Try](../ops/trait.Try.html) for [Option](enum.Option.html)<T>

[Source](../../src/core/option.rs.html#2859)[§](#associatedtype.Output)

#### type [Output](../ops/trait.Try.html#associatedtype.Output) = T

🔬This is a nightly-only experimental API. (`try_trait_v2` [#84277](https://github.com/rust-lang/rust/issues/84277))

The type of the value produced by `?` when _not_ short-circuiting.

[Source](../../src/core/option.rs.html#2860)[§](#associatedtype.Residual)

#### type [Residual](../ops/trait.Try.html#associatedtype.Residual) = [Option](enum.Option.html)<[Infallible](../convert/enum.Infallible.html)>

🔬This is a nightly-only experimental API. (`try_trait_v2` [#84277](https://github.com/rust-lang/rust/issues/84277))

The type of the value passed to [`FromResidual::from_residual`](../ops/trait.FromResidual.html#tymethod.from_residual) as part of `?` when short-circuiting. [Read more](../ops/trait.Try.html#associatedtype.Residual)

[Source](../../src/core/option.rs.html#2863)[§](#method.from_output)

#### fn [from_output](../ops/trait.Try.html#tymethod.from_output)(output: <[Option](enum.Option.html)<T> as [Try](../ops/trait.Try.html)>::[Output](../ops/trait.Try.html#associatedtype.Output)) -> [Option](enum.Option.html)<T>

🔬This is a nightly-only experimental API. (`try_trait_v2` [#84277](https://github.com/rust-lang/rust/issues/84277))

Constructs the type from its `Output` type. [Read more](../ops/trait.Try.html#tymethod.from_output)

[Source](../../src/core/option.rs.html#2868)[§](#method.branch)

#### fn [branch](../ops/trait.Try.html#tymethod.branch)( self, ) -> [ControlFlow](../ops/enum.ControlFlow.html)<<[Option](enum.Option.html)<T> as [Try](../ops/trait.Try.html)>::[Residual](../ops/trait.Try.html#associatedtype.Residual), <[Option](enum.Option.html)<T> as [Try](../ops/trait.Try.html)>::[Output](../ops/trait.Try.html#associatedtype.Output)>

🔬This is a nightly-only experimental API. (`try_trait_v2` [#84277](https://github.com/rust-lang/rust/issues/84277))

Used in `?` to decide whether the operator should produce a value (because this returned [`ControlFlow::Continue`](../ops/enum.ControlFlow.html#variant.Continue)) or propagate a value back to the caller (because this returned [`ControlFlow::Break`](../ops/enum.ControlFlow.html#variant.Break)). [Read more](../ops/trait.Try.html#tymethod.branch)

[Source](../../src/core/option.rs.html#2290)[§](#impl-UseCloned-for-Option%3CT%3E)

### impl<T> [UseCloned](../clone/trait.UseCloned.html) for [Option](enum.Option.html)<T>

where T: [UseCloned](../clone/trait.UseCloned.html),

## Auto Trait Implementations[§](#synthetic-implementations)

[§](#impl-Freeze-for-Option%3CT%3E)

### impl<T> [Freeze](../marker/trait.Freeze.html) for [Option](enum.Option.html)<T>

where T: [Freeze](../marker/trait.Freeze.html),

[§](#impl-RefUnwindSafe-for-Option%3CT%3E)

### impl<T> [RefUnwindSafe](../panic/trait.RefUnwindSafe.html) for [Option](enum.Option.html)<T>

where T: [RefUnwindSafe](../panic/trait.RefUnwindSafe.html),

[§](#impl-Send-for-Option%3CT%3E)

### impl<T> [Send](../marker/trait.Send.html) for [Option](enum.Option.html)<T>

where T: [Send](../marker/trait.Send.html),

[§](#impl-Sync-for-Option%3CT%3E)

### impl<T> [Sync](../marker/trait.Sync.html) for [Option](enum.Option.html)<T>

where T: [Sync](../marker/trait.Sync.html),

[§](#impl-Unpin-for-Option%3CT%3E)

### impl<T> [Unpin](../marker/trait.Unpin.html) for [Option](enum.Option.html)<T>

where T: [Unpin](../marker/trait.Unpin.html),

[§](#impl-UnsafeUnpin-for-Option%3CT%3E)

### impl<T> [UnsafeUnpin](../marker/trait.UnsafeUnpin.html) for [Option](enum.Option.html)<T>

where T: [UnsafeUnpin](../marker/trait.UnsafeUnpin.html),

[§](#impl-UnwindSafe-for-Option%3CT%3E)

### impl<T> [UnwindSafe](../panic/trait.UnwindSafe.html) for [Option](enum.Option.html)<T>

where T: [UnwindSafe](../panic/trait.UnwindSafe.html),

## Blanket Implementations[§](#blanket-implementations)

[Source](../../src/core/any.rs.html#141)[§](#impl-Any-for-T)

### impl<T> [Any](../any/trait.Any.html) for T

where T: 'static + ?[Sized](../marker/trait.Sized.html),

[Source](../../src/core/any.rs.html#142)[§](#method.type_id)

#### fn [type_id](../any/trait.Any.html#tymethod.type_id)(&self) -> [TypeId](../any/struct.TypeId.html)

Gets the `TypeId` of `self`. [Read more](../any/trait.Any.html#tymethod.type_id)

[Source](../../src/core/borrow.rs.html#212)[§](#impl-Borrow%3CT%3E-for-T)

### impl<T> [Borrow](../borrow/trait.Borrow.html)<T> for T

where T: ?[Sized](../marker/trait.Sized.html),

[Source](../../src/core/borrow.rs.html#214)[§](#method.borrow)

#### fn [borrow](../borrow/trait.Borrow.html#tymethod.borrow)(&self) -> [&T](../primitive.reference.html)

Immutably borrows from an owned value. [Read more](../borrow/trait.Borrow.html#tymethod.borrow)

[Source](../../src/core/borrow.rs.html#221)[§](#impl-BorrowMut%3CT%3E-for-T)

### impl<T> [BorrowMut](../borrow/trait.BorrowMut.html)<T> for T

where T: ?[Sized](../marker/trait.Sized.html),

[Source](../../src/core/borrow.rs.html#222)[§](#method.borrow_mut)

#### fn [borrow_mut](../borrow/trait.BorrowMut.html#tymethod.borrow_mut)(&mut self) -> [&mut T](../primitive.reference.html)

Mutably borrows from an owned value. [Read more](../borrow/trait.BorrowMut.html#tymethod.borrow_mut)

[Source](../../src/core/clone.rs.html#648)[§](#impl-CloneToUninit-for-T)

### impl<T> [CloneToUninit](../clone/trait.CloneToUninit.html) for T

where T: [Clone](../clone/trait.Clone.html),

[Source](../../src/core/clone.rs.html#650)[§](#method.clone_to_uninit)

#### unsafe fn [clone_to_uninit](../clone/trait.CloneToUninit.html#tymethod.clone_to_uninit)(&self, dest: [*mut](../primitive.pointer.html)[u8](../primitive.u8.html))

🔬This is a nightly-only experimental API. (`clone_to_uninit` [#126799](https://github.com/rust-lang/rust/issues/126799))

Performs copy-assignment from `self` to `dest`. [Read more](../clone/trait.CloneToUninit.html#tymethod.clone_to_uninit)

[Source](../../src/core/convert/mod.rs.html#805)[§](#impl-From%3C!%3E-for-T)

### impl<T> [From](../convert/trait.From.html)<[!](../primitive.never.html)> for T

[Source](../../src/core/convert/mod.rs.html#806)[§](#method.from-3)

#### fn [from](../convert/trait.From.html#tymethod.from)(t: [!](../primitive.never.html)) -> T

Converts to this type from the input type.

[Source](../../src/core/convert/mod.rs.html#788)[§](#impl-From%3CT%3E-for-T)

### impl<T> [From](../convert/trait.From.html)<T> for T

[Source](../../src/core/convert/mod.rs.html#791)[§](#method.from-4)

#### fn [from](../convert/trait.From.html#tymethod.from)(t: T) -> T

Returns the argument unchanged.

[Source](../../src/core/convert/mod.rs.html#770-772)[§](#impl-Into%3CU%3E-for-T)

### impl<T, U> [Into](../convert/trait.Into.html)<U> for T

where U: [From](../convert/trait.From.html)<T>,

[Source](../../src/core/convert/mod.rs.html#780)[§](#method.into)

#### fn [into](../convert/trait.Into.html#tymethod.into)(self) -> U

Calls `U::from(self)`.

That is, this conversion is whatever the implementation of `[From](../convert/trait.From.html)<T> for U` chooses to do.

[Source](../../src/alloc/borrow.rs.html#72-74)[§](#impl-ToOwned-for-T)

### impl<T> [ToOwned](../borrow/trait.ToOwned.html) for T

where T: [Clone](../clone/trait.Clone.html),

[Source](../../src/alloc/borrow.rs.html#76)[§](#associatedtype.Owned)

#### type [Owned](../borrow/trait.ToOwned.html#associatedtype.Owned) = T

The resulting type after obtaining ownership.

[Source](../../src/alloc/borrow.rs.html#77)[§](#method.to_owned)

#### fn [to_owned](../borrow/trait.ToOwned.html#tymethod.to_owned)(&self) -> T

Creates owned data from borrowed data, usually by cloning. [Read more](../borrow/trait.ToOwned.html#tymethod.to_owned)

[Source](../../src/alloc/borrow.rs.html#81)[§](#method.clone_into)

#### fn [clone_into](../borrow/trait.ToOwned.html#method.clone_into)(&self, target: [&mut T](../primitive.reference.html))

Uses borrowed data to replace owned data, usually by cloning. [Read more](../borrow/trait.ToOwned.html#method.clone_into)

[Source](../../src/core/convert/mod.rs.html#830-832)[§](#impl-TryFrom%3CU%3E-for-T)

### impl<T, U> [TryFrom](../convert/trait.TryFrom.html)<U> for T

where U: [Into](../convert/trait.Into.html)<T>,

[Source](../../src/core/convert/mod.rs.html#834)[§](#associatedtype.Error)

#### type [Error](../convert/trait.TryFrom.html#associatedtype.Error) = [Infallible](../convert/enum.Infallible.html)

The type returned in the event of a conversion error.

[Source](../../src/core/convert/mod.rs.html#837)[§](#method.try_from)

#### fn [try_from](../convert/trait.TryFrom.html#tymethod.try_from)(value: U) -> [Result](../result/enum.Result.html)<T, <T as [TryFrom](../convert/trait.TryFrom.html)<U>>::[Error](../convert/trait.TryFrom.html#associatedtype.Error)>

Performs the conversion.

[Source](../../src/core/convert/mod.rs.html#814-816)[§](#impl-TryInto%3CU%3E-for-T)

### impl<T, U> [TryInto](../convert/trait.TryInto.html)<U> for T

where U: [TryFrom](../convert/trait.TryFrom.html)<T>,

[Source](../../src/core/convert/mod.rs.html#818)[§](#associatedtype.Error-1)

#### type [Error](../convert/trait.TryInto.html#associatedtype.Error) = <U as [TryFrom](../convert/trait.TryFrom.html)<T>>::[Error](../convert/trait.TryFrom.html#associatedtype.Error)

The type returned in the event of a conversion error.

[Source](../../src/core/convert/mod.rs.html#821)[§](#method.try_into)

#### fn [try_into](../convert/trait.TryInto.html#tymethod.try_into)(self) -> [Result](../result/enum.Result.html)<U, <U as [TryFrom](../convert/trait.TryFrom.html)<T>>::[Error](../convert/trait.TryFrom.html#associatedtype.Error)>

Performs the conversion.
