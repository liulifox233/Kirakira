//! An object's member table, ported from the reference `tTJSCustomObject`.
//!
//! TJS2 keeps an object's members in an open-chain hash table, and
//! `EnumMembers` walks that table's buckets, so an object's enumeration order
//! is the order of the *slots*, not of the keys.  The whole store is ported
//! here so this engine enumerates an object or a `Dictionary` in exactly the
//! sequence the reference would for the same history of member operations:
//!
//! - **Hash function** — `tTJSHashFunc<tjs_char *>::Make` (`tjsHashSearch.h:78-98`),
//!   a One-at-a-Time mix over UTF-16 code units (`tjs_char` is a 16 bit unit
//!   on the Windows reference, `tjsTypes.h:63-66`), stopping at the first NUL
//!   and mapping a zero result to `0xffffffff`.  See [`hash_name`].
//! - **Slots** — `Symbols` is an array of `HashSize` slots (`tjsObject.h:479-481`),
//!   `HashSize = 1 << hashbits` and `HashMask = HashSize - 1`
//!   (`tjsObject.cpp:390-399`).  A slot either holds a symbol itself (the
//!   bucket *head*) or is unused, and a symbol that collides with an occupied
//!   slot hangs off it in a singly linked `Next` chain.  This port stores the
//!   chain newest-last, so enumeration reverses it; the head is separate
//!   because the reference emits it *after* its chain.
//! - **Insertion** — `tTJSCustomObject::Add` (`tjsObject.cpp:570-660`): an
//!   existing member keeps its slot and only its value is replaced; a new
//!   member takes the slot itself when the slot is free, otherwise it is
//!   pushed onto the *front* of that slot's chain.
//! - **Lookup** — `tTJSCustomObject::Find` (`tjsObject.cpp:1089-1185`): the
//!   chain is searched from its front, then the head.  A member found at chain
//!   position 3 or later is moved to the front (`cnt > 2`, `:1110-1118`), so
//!   reads feed back into the enumeration order.  This engine has no bytecode
//!   hash hints, so only the hint-less search path is modelled.
//! - **Deletion** — `tTJSCustomObject::DeleteByName` (`tjsObject.cpp:856-891`):
//!   deleting the head marks the slot unused (`PostClear`, `tjsObject.h:451-458`)
//!   and leaves its chain in place -- a later insert into that bucket takes the
//!   slot and sits in front of the surviving chain.  Deleting a chain entry
//!   severs and drops it.
//! - **Enumeration** — `tTJSCustomObject::InternalEnumMembers`
//!   (`tjsObject.cpp:1207-1240`), reached through `EnumMembers` (`:1652-1656`):
//!   for every bucket in index order, walk its chain front-to-back, then emit
//!   the head.
//! - **Resizing** — `tTJSCustomObject::RebuildHash(requestcount)`
//!   (`tjsObject.cpp:748-848`): the new size is [`hash_bits_for_count`] of the
//!   requested member count, and a request that lands on the current size is a
//!   no-op.  The rebuild walks the *old* table in enumeration order and
//!   re-inserts each symbol into the new table, which prepends within the new
//!   buckets and therefore reverses the relative order of symbols that share
//!   one.  The size therefore only changes on the exact call sites the
//!   reference rebuilds from:
//!   - a lazy rebuild inside `PropGet` when the process-global rebuild magic
//!     changed (`tjsObject.cpp:1396-1398`); the magic is bumped by
//!     `TJSDoRehash()` (`:361-362`) from `Scripts.rehash`
//!     (scriptsEx `Main.cpp:628`) and from the engine's own 1500 ms tick
//!     (`SystemControl.cpp:176-180`),
//!   - `Dictionary.assign` / `Dictionary.assignStruct`
//!     (`tjsDictionary.cpp:334`, `:355`, `:549`),
//!   - binary struct deserialization (`tjsBinarySerializer.cpp:79-90`),
//!   - `new Dictionary(count)` sizes the table up front
//!     (`tjsDictionary.cpp:249-257`), and a plain object starts at
//!     `TJS_NAMESPACE_DEFAULT_HASH_BITS` = 3 bits, i.e. 8 buckets
//!     (`tjsObject.h:373`, `:505`).
//!
//! krkr2 trunk (`krkr2/kirikiri2/trunk/kirikiri2/src/core/tjs2/`) is
//! byte-identical here apart from `bool` casts; its rehash tick lives in the
//! window message loop (`MainFormUnit.cpp:713-719`) instead of
//! `SystemControl`.  Kirikiroid2 carries the same table code.
//!
//! The expected sequences in this module's tests are not hand-derived: they
//! are the output of `tests/reference/enumeration_order_oracle.cpp`, a
//! standalone harness carrying the reference functions above verbatim for the
//! same operation sequences (build instructions are in its header).
//!
//! One difference from the reference is out of reach here and documented
//! rather than approximated: the reference's tick skips while
//! `ContinuousEventCalling` is set (`SystemControl.cpp:70-90`,
//! `:169-180`).  This engine has no continuous-event mode, so
//! [`Runtime::tjs_rehash_tick`](crate::runtime::Runtime::tjs_rehash_tick)
//! applies the 1500 ms rule unconditionally.

use std::cell::{Cell, RefCell};

use crate::runtime::value::Variant;

/// `TJS_NAMESPACE_DEFAULT_HASH_BITS` (`tjsObject.h:373`): a plain
/// `tTJSCustomObject`, and therefore every `new Object()`/instance, starts
/// with `1 << 3` buckets.
pub const DEFAULT_HASH_BITS: u32 = 3;

/// `TJSObjectHashBitsLimit` (`tjsObject.cpp:370`).
pub const HASH_BITS_LIMIT: u32 = 32;

/// Upper bound on the number of buckets this port will actually allocate.
///
/// The reference caps at [`HASH_BITS_LIMIT`] and lets `operator new` fail
/// (`std::bad_alloc`, which becomes a TJS exception) above that; a Rust
/// allocation failure aborts the process instead, and a script-reachable
/// `new Dictionary(huge)` must not be able to do that.  Sizes are only clamped
/// at 16.7M buckets (about 500 MB), far above any table a real member count
/// reaches -- `hash_bits_for_count` returns 2 bits more than `log2(count)`.
const HASH_BITS_ALLOCATION_CAP: u32 = 24;

/// The reference's rehash interval: `tick > LastRehashedTick + 1500`
/// (`SystemControl.cpp:176`), milliseconds.
pub const REHASH_TICK_INTERVAL_MILLIS: i64 = 1500;

thread_local! {
    /// `TJSGlobalRebuildHashMagic` (`tjsObject.cpp:361`): process-global in
    /// the reference; the engine runs script on one thread, and a per-thread
    /// counter keeps parallel tests from rehashing each other's objects.
    static REBUILD_HASH_MAGIC: Cell<u32> = const { Cell::new(0) };
}

/// The current value of `TJSGlobalRebuildHashMagic`.
pub fn rebuild_hash_magic() -> u32 {
    REBUILD_HASH_MAGIC.with(Cell::get)
}

/// `TJSDoRehash` (`tjsObject.cpp:362`): mark every object's table as stale.
/// Each object rebuilds lazily, on its next member read (`:1396-1398`).
pub fn tjs_do_rehash() {
    REBUILD_HASH_MAGIC.with(|magic| magic.set(magic.get().wrapping_add(1)));
}

/// `tTJSHashFunc<tjs_char *>::Make` (`tjsHashSearch.h:78-98`).
pub fn hash_name(name: &str) -> u32 {
    let mut ret: u32 = 0;
    for unit in name.encode_utf16() {
        ret = ret.wrapping_add(u32::from(unit));
        ret = ret.wrapping_add(ret << 10);
        ret ^= ret >> 6;
    }
    ret = ret.wrapping_add(ret << 3);
    ret ^= ret >> 11;
    ret = ret.wrapping_add(ret << 15);
    if ret == 0 {
        ret = u32::MAX;
    }
    ret
}

/// The size a table holding `requestcount` members gets: the expression shared
/// by `tTJSCustomObject::RebuildHash` (`tjsObject.cpp:750-762`) and
/// `tTJSDictionaryClass::CreateNew` (`tjsDictionary.cpp:249-257`).
pub fn hash_bits_for_count(requestcount: i64) -> u32 {
    let mut v = requestcount.clamp(0, i64::from(i32::MAX)) as i32;
    let mut r: i32 = 0;
    if v & 0xffff_0000u32 as i32 != 0 {
        r = 16;
        v >>= 16;
    }
    if v & 0xff00 != 0 {
        r += 8;
        v >>= 8;
    }
    if v & 0xf0 != 0 {
        r += 4;
        v >>= 4;
    }
    v <<= 1;
    let bits = r + ((0xffff_aa50u32 >> (v as u32 & 31)) as i32 & 0x03) + 2;
    bits.clamp(0, HASH_BITS_LIMIT as i32) as u32
}

/// Where a found member lives, mirroring the three places `Find` can return.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Slot {
    /// `Symbols[bucket]` itself.
    Head(usize),
    /// `Symbols[bucket].Next...`, at chain position `index` counted from the
    /// front of the chain.
    Chain(usize, usize),
}

#[derive(Clone, Debug)]
struct Symbol {
    name: String,
    hash: u32,
    value: Variant,
}

/// One slot of `Symbols`, plus the chain hanging off it.
///
/// `chain` stores the reference's `Next` list tail-first, so the newest
/// inserted member -- the front of the reference chain -- is its last element.
#[derive(Clone, Debug, Default)]
struct Bucket {
    head: Option<Box<Symbol>>,
    chain: Vec<Symbol>,
}

#[derive(Clone, Debug)]
struct Table {
    buckets: Vec<Bucket>,
    count: usize,
    /// `RebuildHashMagic` (`tjsObject.h:482`), compared against
    /// [`rebuild_hash_magic`] on every read.
    rebuild_magic: u32,
}

impl Table {
    fn new(hashbits: u32) -> Self {
        let hashbits = hashbits.min(HASH_BITS_LIMIT).min(HASH_BITS_ALLOCATION_CAP);
        let size = 1usize << hashbits;
        Self {
            buckets: vec![Bucket::default(); size],
            count: 0,
            rebuild_magic: rebuild_hash_magic(),
        }
    }

    fn mask(&self) -> usize {
        self.buckets.len() - 1
    }

    fn bucket_of(&self, hash: u32) -> usize {
        (hash as usize) & self.mask()
    }

    /// `tTJSCustomObject::Find` (`tjsObject.cpp:1131-1184`), hint-less path.
    fn find_slot(table: &mut Table, name: &str) -> Option<Slot> {
        let hash = hash_name(name);
        let bucket = table.bucket_of(hash);
        if table.buckets[bucket].head.is_none() && table.buckets[bucket].chain.is_empty() {
            return None;
        }
        // The chain is stored tail-first, so `position` runs over it in the
        // reference's front-to-back order.
        let chain_len = table.buckets[bucket].chain.len();
        for position in 0..chain_len {
            let index = chain_len - 1 - position;
            let hit = {
                let symbol = &table.buckets[bucket].chain[index];
                symbol.hash == hash && symbol.name == name
            };
            if hit {
                if position > 2 {
                    // `if(cnt>2) { move to first }` (`tjsObject.cpp:1110-1118`).
                    let symbol = table.buckets[bucket].chain.remove(index);
                    table.buckets[bucket].chain.push(symbol);
                    return Some(Slot::Chain(bucket, 0));
                }
                return Some(Slot::Chain(bucket, position));
            }
        }
        if let Some(head) = &table.buckets[bucket].head
            && head.hash == hash
            && head.name == name
        {
            return Some(Slot::Head(bucket));
        }
        None
    }

    fn slot_value(table: &Table, slot: Slot) -> Variant {
        match slot {
            Slot::Head(bucket) => table.buckets[bucket]
                .head
                .as_ref()
                .expect("head slot resolved by find")
                .value
                .clone(),
            Slot::Chain(bucket, position) => {
                let chain_len = table.buckets[bucket].chain.len();
                table.buckets[bucket].chain[chain_len - 1 - position]
                    .value
                    .clone()
            }
        }
    }

    /// `Add` (`tjsObject.cpp:570-660`): replace in place, or insert new.
    fn add(table: &mut Table, name: String, value: Variant) {
        if let Some(slot) = Table::find_slot(table, &name) {
            match slot {
                Slot::Head(bucket) => {
                    table.buckets[bucket]
                        .head
                        .as_mut()
                        .expect("head slot resolved by find")
                        .value = value;
                }
                Slot::Chain(bucket, position) => {
                    let chain_len = table.buckets[bucket].chain.len();
                    table.buckets[bucket].chain[chain_len - 1 - position].value = value;
                }
            }
            return;
        }
        let hash = hash_name(&name);
        let bucket = table.bucket_of(hash);
        let symbol = Symbol { name, hash, value };
        if table.buckets[bucket].head.is_some() {
            // `make a chain and insert it after lv1` -- the front of the chain.
            table.buckets[bucket].chain.push(symbol);
        } else {
            table.buckets[bucket].head = Some(Box::new(symbol));
        }
        table.count += 1;
    }

    /// `DeleteByName` (`tjsObject.cpp:856-891`).
    fn delete(table: &mut Table, name: &str) -> bool {
        let hash = hash_name(name);
        let bucket = table.bucket_of(hash);
        if table.buckets[bucket].head.is_none() && table.buckets[bucket].chain.is_empty() {
            return false;
        }
        if table.buckets[bucket]
            .head
            .as_ref()
            .is_some_and(|head| head.name == name)
        {
            // `PostClear` marks the slot unused; the chain stays behind it.
            table.buckets[bucket].head = None;
            table.count -= 1;
            return true;
        }
        let chain_len = table.buckets[bucket].chain.len();
        for position in 0..chain_len {
            let index = chain_len - 1 - position;
            let hit = {
                let symbol = &table.buckets[bucket].chain[index];
                symbol.hash == hash && symbol.name == name
            };
            if hit {
                table.buckets[bucket].chain.remove(index);
                table.count -= 1;
                return true;
            }
        }
        false
    }

    /// `InternalEnumMembers` (`tjsObject.cpp:1207-1240`): bucket order, each
    /// bucket's chain front-to-back, then the head.
    fn for_each<F: FnMut(&Symbol)>(&self, mut visit: F) {
        for bucket in &self.buckets {
            for symbol in bucket.chain.iter().rev() {
                visit(symbol);
            }
            if let Some(head) = &bucket.head {
                visit(head);
            }
        }
    }

    /// `AddTo` (`tjsObject.cpp:632-698`), used by [`Table::rebuild`].
    fn add_to(buckets: &mut [Bucket], mask: usize, symbol: Symbol) {
        let bucket = (symbol.hash as usize) & mask;
        if buckets[bucket].head.is_some() {
            buckets[bucket].chain.push(symbol);
        } else {
            buckets[bucket].head = Some(Box::new(symbol));
        }
    }

    /// `RebuildHash(requestcount)` (`tjsObject.cpp:743-848`).
    fn rebuild(table: &mut Table, requestcount: i64) {
        let hashbits = hash_bits_for_count(requestcount).min(HASH_BITS_ALLOCATION_CAP);
        let new_size = 1usize << hashbits;
        if new_size == table.buckets.len() {
            return;
        }
        let mask = new_size - 1;
        let mut buckets = vec![Bucket::default(); new_size];
        let mut symbols = Vec::with_capacity(table.count);
        table.for_each(|symbol| symbols.push(symbol.clone()));
        for symbol in symbols {
            Table::add_to(&mut buckets, mask, symbol);
        }
        table.buckets = buckets;
    }
}

/// An object's member store: the reference's `Symbols`/`HashSize`/`Count`.
#[derive(Clone, Debug)]
pub struct SymbolTable {
    table: RefCell<Table>,
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::with_hash_bits(DEFAULT_HASH_BITS)
    }
}

impl SymbolTable {
    /// A table with `1 << hashbits` buckets, the reference's
    /// `tTJSCustomObject(hashbits)` (`tjsObject.cpp:390-399`).
    pub fn with_hash_bits(hashbits: u32) -> Self {
        Self {
            table: RefCell::new(Table::new(hashbits)),
        }
    }

    /// A table the size `new Dictionary(count)` would be built with
    /// (`tjsDictionaryClass::CreateNew`, `tjsDictionary.cpp:249-257`).
    pub fn with_dictionary_count(count: i64) -> Self {
        Self::with_hash_bits(hash_bits_for_count(count))
    }

    /// The number of buckets, for diagnostics and tests.
    pub fn hash_size(&self) -> usize {
        self.table.borrow().buckets.len()
    }

    /// `Count` (`tjsObject.h:478`): the number of live members.
    pub fn len(&self) -> usize {
        self.table.borrow().count
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `PropGet`'s member lookup (`tjsObject.cpp:1390-1420`): rebuild the
    /// table first when the global rebuild magic moved on, then `Find` -- which
    /// moves a member found at chain position 3 or later to the chain front.
    pub fn get(&self, name: &str) -> Option<Variant> {
        let mut table = self.table.borrow_mut();
        Self::rebuild_if_stale(&mut table);
        let slot = Table::find_slot(&mut table, name)?;
        Some(Table::slot_value(&table, slot))
    }

    /// A read that only asks whether the member is there.  The reference has
    /// no separate test -- a probe is a `PropGet` -- so this is a `Find` with
    /// the same rebuild and move-to-front effects.
    pub fn contains(&self, name: &str) -> bool {
        let mut table = self.table.borrow_mut();
        Self::rebuild_if_stale(&mut table);
        Table::find_slot(&mut table, name).is_some()
    }

    /// `PropSet`-with-`TJS_MEMBERENSURE` / `Add` (`tjsObject.cpp:570-660`).
    /// Not a `PropGet`, so the reference does not check the rebuild magic here.
    pub fn insert(&self, name: impl Into<String>, value: Variant) {
        let mut table = self.table.borrow_mut();
        Table::add(&mut table, name.into(), value);
    }

    /// `DeleteMember` / `DeleteByName` (`tjsObject.cpp:856-891`).  Answers
    /// whether the member was there, like the reference's `bool` result.
    pub fn remove(&self, name: &str) -> bool {
        let mut table = self.table.borrow_mut();
        Table::delete(&mut table, name)
    }

    /// `Clear()` (`tjsObject.h:566`) is `DeleteAllMembers()`
    /// (`tjsObject.cpp:896-1000`): every chain and head goes away and `Count`
    /// drops to zero, while `HashSize` stays as it was.
    pub fn clear(&self) {
        let mut table = self.table.borrow_mut();
        for bucket in &mut table.buckets {
            bucket.head = None;
            bucket.chain.clear();
        }
        table.count = 0;
    }

    /// `EnumMembers`: every live member, in bucket order.
    pub fn entries(&self) -> Vec<(String, Variant)> {
        let table = self.table.borrow();
        let mut entries = Vec::with_capacity(table.count);
        table.for_each(|symbol| entries.push((symbol.name.clone(), symbol.value.clone())));
        entries
    }

    /// `RebuildHash(requestcount)` (`tjsObject.cpp:743-848`).
    pub fn rebuild(&self, requestcount: i64) {
        let mut table = self.table.borrow_mut();
        Table::rebuild(&mut table, requestcount);
    }

    fn rebuild_if_stale(table: &mut Table) {
        if table.rebuild_magic != rebuild_hash_magic() {
            Table::rebuild(table, table.count as i64);
        }
    }

    /// Forces the lazy rebuild `PropGet` performs when the magic is stale.
    /// Exposed for tests that need to observe the table size.
    #[cfg(test)]
    pub fn rebuild_if_stale_now(&self) {
        let mut table = self.table.borrow_mut();
        SymbolTable::rebuild_if_stale(&mut table);
    }
}

impl PartialEq for SymbolTable {
    /// Members are what an object compares by, not the slots they sit in.
    fn eq(&self, other: &Self) -> bool {
        let this = self.entries();
        let other = other.entries();
        this.len() == other.len()
            && this.iter().all(|(name, value)| {
                other
                    .iter()
                    .any(|(other_name, other_value)| other_name == name && other_value == value)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with(names: &[&str]) -> SymbolTable {
        let table = SymbolTable::default();
        for name in names {
            table.insert(name.to_string(), Variant::Integer(1));
        }
        table
    }

    fn order(table: &SymbolTable) -> Vec<String> {
        table.entries().into_iter().map(|(name, _)| name).collect()
    }

    /// Hash values produced by the reference function itself, run over the
    /// same names (a standalone harness carrying `tTJSHashFunc<tjs_char *>`
    /// verbatim over 16 bit code units).
    #[test]
    fn hash_matches_the_reference_function() {
        assert_eq!(hash_name(""), 0xffff_ffff);
        assert_eq!(hash_name("one"), 0x7f17_f79a);
        assert_eq!(hash_name("two"), 0x57d0_3706);
        assert_eq!(hash_name("a"), 0xca2e_9442);
        assert_eq!(hash_name("e"), 0x1116_2210);
        assert_eq!(hash_name("b"), 0x00db_819b);
        assert_eq!(hash_name("c"), 0xeeba_5d59);
        // The hash runs over UTF-16 code units: a non-BMP character hashes as
        // its surrogate pair (the reference's `tjs_char` is 16 bit on Windows).
        assert_eq!(hash_name("\u{1f600}"), 0x5c28_937a);
    }

    /// Bucket counts the reference arithmetic produces for a member count
    /// (`tjsObject.cpp:750-762`).
    #[test]
    fn hash_bits_grow_by_the_reference_formula() {
        for (count, bits) in [
            (0, 2),
            (1, 2),
            (2, 3),
            (4, 4),
            (8, 5),
            (16, 6),
            (32, 7),
            (64, 8),
        ] {
            assert_eq!(hash_bits_for_count(count), bits, "count {count}");
        }
        assert_eq!(SymbolTable::with_dictionary_count(16).hash_size(), 64);
    }

    /// The bucket walk: ascending bucket index, and a slot's chain before the
    /// slot itself.  Buckets of `e`(0), `c`(1), `a`(2), `b`(3) with 8 slots --
    /// neither sorted (`a,b,c,e`) nor insertion (`e,a,c,b`) order.
    #[test]
    fn enumeration_walks_buckets_not_keys_or_insertions() {
        assert_eq!(
            order(&table_with(&["e", "a", "c", "b"])),
            ["e", "c", "a", "b"]
        );
    }

    /// Colliding names share a slot: the first takes the slot, later ones go
    /// to the front of its chain, and the slot itself is emitted last
    /// (`tjsObject.cpp:1207-1240`).
    #[test]
    fn colliding_names_chain_newest_first_with_the_slot_head_last() {
        assert_eq!(order(&table_with(&["e", "l", "w"])), ["w", "l", "e"]);
    }

    /// Deleting the slot head leaves the chain behind; a later insert into the
    /// same bucket takes the freed slot and sits in front of that chain.
    #[test]
    fn deleting_a_bucket_head_keeps_its_chain() {
        let table = table_with(&["e", "l", "w"]);
        assert!(table.remove("e"));
        table.insert("y".to_string(), Variant::Integer(1));
        assert_eq!(order(&table), ["w", "l", "y"]);
    }

    /// Deleting a chain entry severs exactly that entry.
    #[test]
    fn deleting_a_chain_entry_keeps_the_rest() {
        let table = table_with(&["e", "l", "w"]);
        assert!(table.remove("l"));
        assert_eq!(order(&table), ["w", "e"]);
    }

    /// `Find` moves a member found at chain position 3 or later to the front
    /// (`cnt > 2`), and leaves one at position 2 alone.
    #[test]
    fn reads_move_a_deep_chain_member_to_the_front() {
        let table = table_with(&["e", "l", "w", "y", "ab"]);
        assert_eq!(order(&table), ["ab", "y", "w", "l", "e"]);
        assert!(table.get("ab").is_some()); // position 0: no move
        assert_eq!(order(&table), ["ab", "y", "w", "l", "e"]);
        assert!(table.get("y").is_some()); // position 1: no move
        assert_eq!(order(&table), ["ab", "y", "w", "l", "e"]);
        assert!(table.get("l").is_some()); // position 3: moves
        assert_eq!(order(&table), ["l", "ab", "y", "w", "e"]);
    }

    /// `RebuildHash` re-inserts in enumeration order, which reverses the
    /// relative order inside each new bucket; a request that lands on the
    /// current size is a no-op (`tjsObject.cpp:764`).
    #[test]
    fn rebuild_resizes_and_reorders_like_the_reference() {
        let table = table_with(&["e", "a", "c", "b", "e2", "l2", "w2", "y2"]);
        assert_eq!(
            order(&table),
            ["e2", "e", "c", "l2", "a", "b", "w2", "y2"],
            "8 buckets"
        );
        table.rebuild(3); // hash bits for 3 members = 3, i.e. the same 8 slots
        assert_eq!(
            order(&table),
            ["e2", "e", "c", "l2", "a", "b", "w2", "y2"],
            "same-size rebuild is a no-op"
        );
        table.rebuild(16); // 64 buckets
        assert_eq!(
            order(&table),
            ["e2", "a", "e", "w2", "c", "b", "l2", "y2"],
            "64 buckets"
        );
    }

    /// A table built at `new Dictionary(16)`'s size enumerates the same
    /// insertions differently from a default 8-bucket object, and a rebuild to
    /// that size reproduces the 64-bucket order.
    #[test]
    fn dictionary_creation_size_changes_the_order() {
        let names = ["e", "a", "c", "b", "e2", "l2", "w2", "y2"];
        let default_table = table_with(&names);
        let sized = SymbolTable::with_dictionary_count(16);
        for name in names {
            sized.insert(name.to_string(), Variant::Integer(1));
        }
        assert_eq!(order(&sized), ["e2", "a", "e", "w2", "c", "b", "l2", "y2"]);
        let object = table_with(&names);
        object.rebuild(16);
        assert_eq!(order(&object), order(&sized));
        assert_ne!(order(&default_table), order(&sized));
    }

    /// A deleted bucket leaves its freed slot for the next insert, and a
    /// re-inserted name lands at that slot instead of the chain.
    #[test]
    fn delete_then_reinsert_lands_on_the_freed_slot() {
        let table = table_with(&["e", "l", "w", "y"]);
        for name in ["e", "l", "w", "y"] {
            assert!(table.remove(name));
        }
        assert_eq!(order(&table), Vec::<String>::new());
        table.insert("e".to_string(), Variant::Integer(1));
        table.insert("l".to_string(), Variant::Integer(1));
        assert_eq!(order(&table), ["l", "e"]);
    }

    /// `Clear` drops every member but keeps the bucket count
    /// (`tjsObject.h:566`, `tjsObject.cpp:896-1000`).
    #[test]
    fn clear_keeps_the_bucket_count() {
        let table = table_with(&["e", "l", "w", "y"]);
        table.rebuild(64);
        table.clear();
        assert_eq!(table.len(), 0);
        assert_eq!(table.hash_size(), 256);
        assert_eq!(order(&table), Vec::<String>::new());
    }

    /// A stale global magic rebuilds the table at the member count on the next
    /// read (`tjsObject.cpp:1396-1398`); an insert alone does not.
    #[test]
    fn a_read_rebuilds_a_stale_table() {
        let table = table_with(&["e", "a", "c", "b", "e2", "l2", "w2", "y2"]);
        assert_eq!(table.hash_size(), 8);
        tjs_do_rehash();
        table.insert("z".to_string(), Variant::Integer(1));
        assert_eq!(table.hash_size(), 8, "an insert is not a PropGet");
        assert!(table.get("z").is_some());
        assert_eq!(table.hash_size(), 32, "9 members rebuild to 32 buckets");
    }

    /// The engine's tick is the reference's `if(tick > LastRehashedTick + 1500)
    /// { LastRehashedTick = tick; TJSDoRehash(); }` (`SystemControl.cpp:176-180`)
    /// with `LastRehashedTick` starting at 0 (`:51`), and its effect on the
    /// table arrives with the next read.
    #[test]
    fn the_engine_tick_bumps_the_magic_after_1500_milliseconds() {
        use crate::runtime::Runtime;

        let mut runtime = Runtime::new();
        let handle = runtime.alloc_dictionary_object();
        for name in ["e", "a", "c", "b", "e2", "l2", "w2", "y2"] {
            runtime.set_object_member(handle, name, Variant::Integer(1));
        }
        let names = |runtime: &Runtime| {
            runtime.heap[handle.0]
                .members
                .entries()
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        let before = names(&runtime);
        assert_eq!(runtime.heap[handle.0].members.hash_size(), 8);

        // `tick > 0 + 1500` is strict: the 1500 ms boundary itself is silent.
        runtime.tjs_rehash_tick(1_500);
        assert_eq!(runtime.heap[handle.0].members.hash_size(), 8);
        // One millisecond later the magic moves; the rebuild itself waits for
        // a read.
        runtime.tjs_rehash_tick(1_501);
        assert_eq!(runtime.heap[handle.0].members.hash_size(), 8);
        assert_eq!(names(&runtime), before, "no rebuild without a read");
        assert_eq!(
            runtime.heap[handle.0].members.get("e"),
            Some(Variant::Integer(1))
        );
        assert_eq!(runtime.heap[handle.0].members.hash_size(), 32);
        assert_ne!(
            names(&runtime),
            before,
            "32 slots order the keys differently"
        );

        // The next tick waits for the next 1500 ms window, and rebuilds at the
        // member count of that moment -- sixteen members land on 64 slots.
        runtime.tjs_rehash_tick(2_900);
        assert_eq!(runtime.heap[handle.0].members.hash_size(), 32);
        for name in ["a3", "b3", "c3", "d3", "e3", "f3", "g3", "h3"] {
            runtime.set_object_member(handle, name, Variant::Integer(1));
        }
        runtime.tjs_rehash_tick(3_002);
        assert!(runtime.heap[handle.0].members.get("e").is_some());
        assert_eq!(runtime.heap[handle.0].members.hash_size(), 64);
    }

    /// Values round-trip through insert/get/remove, and a re-insert replaces
    /// the value in place without disturbing the slot.
    #[test]
    fn values_round_trip_and_reinsertion_keeps_the_slot() {
        let table = SymbolTable::default();
        table.insert("e".to_string(), Variant::Integer(1));
        table.insert("l".to_string(), Variant::Integer(2));
        assert_eq!(order(&table), ["l", "e"]);
        assert_eq!(table.get("e"), Some(Variant::Integer(1)));
        table.insert("e".to_string(), Variant::Integer(9));
        assert_eq!(table.get("e"), Some(Variant::Integer(9)));
        assert_eq!(order(&table), ["l", "e"]);
        assert_eq!(table.len(), 2);
        assert!(table.remove("l"));
        assert!(!table.remove("l"));
        assert_eq!(order(&table), ["e"]);
        assert_eq!(table.len(), 1);
    }
}
