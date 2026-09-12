// Reference oracle for this crate's member-enumeration order.
//
// WHY THIS FILE IS HERE: the expected sequences in
// `crates/krkr-tjs2/src/runtime/symbol_table.rs` and
// `crates/krkr-tjs2/src/runtime/dictionary_tests.rs` are not hand-derived --
// they are the output of this harness for the same operation sequences.  It
// carries the reference implementation's functions *verbatim* (only the
// memory management is adapted to standalone C++), so a reviewer can re-derive
// every fixture without opening the reference tree:
//
//   tjsObject.cpp : Add (:570-660), Find (:1089-1185), DeleteByName (:856-891),
//                   AddTo (:632-698), RebuildHash (:748-848),
//                   InternalEnumMembers (:1207-1240)
//   tjsHashSearch.h : tTJSHashFunc<tjs_char *>::Make (:78-98)
//   tjsObject.h : SymbolData::SelfClear/PostClear/Destory (:412-460)
//   tjsObject.h : SymFlags values`TJS_SYMBOL_USING`/`TJS_SYMBOL_INIT`
//
// Build and run (macOS/Linux, any C++17 compiler):
//
//   clang++ -std=c++17 -O0 -o /tmp/enum-oracle enumeration_order_oracle.cpp
//   /tmp/enum-oracle hashbits                 # the size formula per member count
//   /tmp/enum-oracle hash a b c               # hash value and bucket per name
//   /tmp/enum-oracle ae al aw ay de aab       # ops: a<key> add, d<key> delete,
//                                            #      g<key> get, r<count> rebuild,
//                                            #      b dump buckets
//
// `tjs_char` is `wchar_t` on the Windows reference and therefore 16 bit, so
// the harness hashes UTF-16 code units (`unsigned short`), which is what
// `hash_name` does with `str::encode_utf16`.
//
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>
#include <deque>

typedef uint32_t tjs_uint32;
typedef unsigned short tjs_char;

#define TJS_SYMBOL_USING 0x1
#define TJS_SYMBOL_INIT  0x2

// ---- tjsHashSearch.h:78-98 (specialization for tjs_char *) ----
static tjs_uint32 HashMake(const char16_t *str)
{
    if (!str) return 0;
    tjs_uint32 ret = 0;
    while (*str)
    {
        ret += *str;
        ret += (ret << 10);
        ret ^= (ret >> 6);
        str++;
    }
    ret += (ret << 3);
    ret ^= (ret >> 11);
    ret += (ret << 15);
    if (!ret) ret = (tjs_uint32)-1;
    return ret;
}

struct SymData
{
    std::u16string name;
    tjs_uint32     hash = 0;
    tjs_uint32     flags = 0;   // TJS_SYMBOL_USING / TJS_SYMBOL_INIT
    bool           value = false;  // stand-in for the value slot
};

struct Bucket
{
    SymData head;                 // Symbols[i]
    std::deque<SymData> chain;    // Symbols[i].Next chain, index 0 == lv1->Next
    bool head_init = false;
};

struct Table
{
    std::vector<Bucket> buckets;
    int Count = 0;
    int HashSize = 0, HashMask = 0;
    tjs_uint32 rebuild_magic = 0;

    void Ctor(int hashbits)
    {
        if (hashbits > 32) hashbits = 32;
        HashSize = 1 << hashbits;
        HashMask = HashSize - 1;
        buckets.resize(HashSize);
        for (auto &b : buckets) { b.head.flags = TJS_SYMBOL_INIT; b.head_init = true; }
    }

    void SelfClear(Bucket &b)
    {
        b.head = SymData();
        b.head.flags = TJS_SYMBOL_INIT;
    }
    void PostClear(SymData &d) { d.flags &= ~TJS_SYMBOL_USING; }

    // tjsObject.cpp:1089-1185 Find(), no-hint path only (this engine has no
    // bytecode hash hints; the hint path is the same search with a cached hash).
    SymData *Find(const std::u16string &name, Bucket **out_bucket = nullptr, int *out_index = nullptr)
    {
        tjs_uint32 hash = HashMake(name.c_str());
        Bucket &lv1 = buckets[hash & HashMask];
        if (out_bucket) *out_bucket = &lv1;
        if (!(lv1.head.flags & TJS_SYMBOL_USING) && lv1.chain.empty())
        {
            if (out_index) *out_index = -1;
            return nullptr;
        }
        int cnt = 0;
        for (auto &d : lv1.chain)
        {
            if (d.hash == hash && (d.flags & TJS_SYMBOL_USING) && d.name == name)
            {
                if (cnt > 2)
                {
                    SymData moved = d;
                    lv1.chain.erase(lv1.chain.begin() + cnt);
                    lv1.chain.push_front(moved);
                    if (out_index) *out_index = 0;
                    return &lv1.chain.front();
                }
                if (out_index) *out_index = cnt;
                return &d;
            }
            cnt++;
        }
        if (lv1.head.hash == hash && (lv1.head.flags & TJS_SYMBOL_USING) && lv1.head.name == name)
        {
            if (out_index) *out_index = -2;  // the head slot
            return &lv1.head;
        }
        if (out_index) *out_index = -1;
        return nullptr;
    }

    // tjsObject.cpp:570-629 Add()
    SymData *Add(const std::u16string &name)
    {
        SymData *data = Find(name);
        if (data) return data;
        tjs_uint32 hash = HashMake(name.c_str());
        Bucket &lv1 = buckets[hash & HashMask];
        if (lv1.head.flags & TJS_SYMBOL_USING)
        {
            SymData d;
            d.hash = hash;
            d.flags = TJS_SYMBOL_USING;
            d.name = name;
            lv1.chain.push_front(d);
            data = &lv1.chain.front();
        }
        else
        {
            if (!(lv1.head.flags & TJS_SYMBOL_INIT)) SelfClear(lv1);
            lv1.head.name = name;
            lv1.head.hash = hash;
            lv1.head.flags |= TJS_SYMBOL_USING;
            data = &lv1.head;
        }
        Count++;
        return data;
    }

    // tjsObject.cpp:856-891 DeleteByName()
    bool DeleteByName(const std::u16string &name)
    {
        tjs_uint32 hash = HashMake(name.c_str());
        Bucket &lv1 = buckets[hash & HashMask];
        if (!(lv1.head.flags & TJS_SYMBOL_USING) && lv1.chain.empty()) return false;
        if ((lv1.head.flags & TJS_SYMBOL_USING) && lv1.head.name == name)
        {
            PostClear(lv1.head);
            Count--;
            return true;
        }
        for (size_t i = 0; i < lv1.chain.size(); i++)
        {
            SymData &d = lv1.chain[i];
            if ((d.flags & TJS_SYMBOL_USING) && d.hash == hash && d.name == name)
            {
                lv1.chain.erase(lv1.chain.begin() + i);
                Count--;
                return true;
            }
        }
        return false;
    }

    // tjsObject.cpp:632-698 AddTo(): insert into a fresh table, prepending.
    void AddTo(std::vector<Bucket> &dest, int destmask, const std::u16string &name)
    {
        tjs_uint32 hash = HashMake(name.c_str());
        Bucket &lv1 = dest[hash & destmask];
        if (lv1.head.flags & TJS_SYMBOL_USING)
        {
            SymData d; d.hash = hash; d.flags = TJS_SYMBOL_USING; d.name = name;
            lv1.chain.push_front(d);
        }
        else
        {
            if (!(lv1.head.flags & TJS_SYMBOL_INIT)) { lv1.head = SymData(); lv1.head.flags = TJS_SYMBOL_INIT; }
            lv1.head.name = name; lv1.head.hash = hash; lv1.head.flags |= TJS_SYMBOL_USING;
        }
    }

    static int HashBitsFor(int requestcount)
    {
        int r, v = requestcount;
        if (v & 0xffff0000) r = 16, v >>= 16; else r = 0;
        if (v & 0xff00) r += 8, v >>= 8;
        if (v & 0xf0) r += 4, v >>= 4;
        v <<= 1;
        int newhashbits = r + ((0xffffaa50 >> v) & 0x03) + 2;
        if (newhashbits > 32) newhashbits = 32;
        return newhashbits;
    }

    // tjsObject.cpp:748-848 RebuildHash()
    void RebuildHash(int requestcount)
    {
        int newhashbits = HashBitsFor(requestcount);
        int newhashsize = (1 << newhashbits);
        if (newhashsize == HashSize) return;
        int newhashmask = newhashsize - 1;
        int orgcount = Count;
        std::vector<Bucket> newsymbols(newhashsize);
        for (auto &b : newsymbols) { b.head.flags = TJS_SYMBOL_INIT; }
        std::vector<std::u16string> order;
        for (auto &lv1 : buckets)
        {
            for (auto &d : lv1.chain)
                if (d.flags & TJS_SYMBOL_USING) order.push_back(d.name);
            if (lv1.head.flags & TJS_SYMBOL_USING) order.push_back(lv1.head.name);
        }
        for (auto &n : order) AddTo(newsymbols, newhashmask, n);
        buckets = newsymbols;
        HashSize = newhashsize;
        HashMask = newhashmask;
        Count = orgcount;
    }

    // tjsObject.cpp:1207-1240 InternalEnumMembers()
    std::vector<std::u16string> EnumOrder()
    {
        std::vector<std::u16string> out;
        for (auto &lv1 : buckets)
        {
            for (auto &d : lv1.chain)
                if (d.flags & TJS_SYMBOL_USING) out.push_back(d.name);
            if (lv1.head.flags & TJS_SYMBOL_USING) out.push_back(lv1.head.name);
        }
        return out;
    }
};

static std::u16string U(const char *s)
{
    std::u16string out;
    while (*s) out.push_back((unsigned char)*s++);
    return out;
}

static void dump(Table &t)
{
    auto o = t.EnumOrder();
    printf("[");
    for (size_t i = 0; i < o.size(); i++)
    {
        std::string n;
        for (char16_t c : o[i]) n.push_back((char)c);
        printf("%s%s", i ? ", " : "", n.c_str());
    }
    printf("]\n");
}

static void dump_buckets(Table &t)
{
    for (int i = 0; i < t.HashSize; i++)
    {
        printf("bucket %2d:", i);
        for (auto &d : t.buckets[i].chain)
        {
            std::string n; for (char16_t c : d.name) n.push_back((char)c);
            printf(" [chain %s]", n.c_str());
        }
        if (t.buckets[i].head.flags & TJS_SYMBOL_USING)
        { std::string n; for (char16_t c : t.buckets[i].head.name) n.push_back((char)c);
          printf(" [head %s]", n.c_str()); }
        printf("\n");
    }
}

int main(int argc, char **argv)
{
    if (argc > 1 && !strcmp(argv[1], "hashbits"))
    {
        for (int c = 0; c <= 64; c++) printf("count=%d bits=%d size=%d\n", c, Table::HashBitsFor(c), 1 << Table::HashBitsFor(c));
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "u16"))
    {
        std::u16string u;
        for (int i = 2; i < argc; i++)
        {
            unsigned long cp = strtoul(argv[i], nullptr, 16);
            if (cp > 0xffff) { cp -= 0x10000; u.push_back((char16_t)(0xd800 + (cp >> 10))); u.push_back((char16_t)(0xdc00 + (cp & 0x3ff))); }
            else u.push_back((char16_t)cp);
        }
        printf("hash(cps)=%08x bucket8=%d\n", HashMake(u.c_str()), HashMake(u.c_str()) & 7);
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "hash"))
    {
        for (int i = 2; i < argc; i++)
        {
            std::u16string u = U(argv[i]);
            printf("hash(%s)=%08x bucket8=%d\n", argv[i], HashMake(u.c_str()), HashMake(u.c_str()) & 7);
        }
        return 0;
    }
    // scripted: tokens "a key" add, "d key" delete, "g key" get, "r count" rehash, "b" buckets
    Table t; int start = 1;
    if (argv[1][0] == 'c') { t.Ctor(atoi(argv[1] + 1)); start = 2; }
    else t.Ctor(3);
    for (int i = start; i < argc; i++)
    {
        char op = argv[i][0];
        if (op == 'a') t.Add(U(argv[i] + 1));
        else if (op == 'd') t.DeleteByName(U(argv[i] + 1));
        else if (op == 'g') t.Find(U(argv[i] + 1));
        else if (op == 'r') t.RebuildHash(atoi(argv[i] + 1));
        else if (op == 'b') dump_buckets(t);
    }
    dump(t);
    return 0;
}
