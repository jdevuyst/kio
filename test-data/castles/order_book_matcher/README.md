# order_book_matcher

`order_book_matcher` is a limit-order-book matching engine: the piece of an
exchange that holds resting orders, decides which of them an arriving order
trades against, and at what price.

The book has two sides. **Bids** are orders to buy, ranked by descending price;
**asks** are orders to sell, ranked by ascending price. Both are held in mutable
host arrays, and both are kept in **price-time priority**: the best price ranks
first, and orders at the same price rank by arrival, so the one that has waited
longest sits at the front of its level. The front of a side is therefore always
the best order available on it, and the two ranks together are the only thing
that decides who trades. A resting order carries its id, side, price, original
quantity, remaining quantity, and its arrival sequence -- the last of these is
what breaks ties inside a price level.

An arriving order is **aggressive**: it crosses against the opposite side while
the prices overlap, taking the front of that queue first, and every trade prints
at the **maker's** price -- the resting order named the price when it joined the
queue, and crossing is the arriving order accepting it. A maker that is used up
leaves the book; a maker with more quantity than the aggressor needs stays where
it is with the rest. A limit order rests with whatever it could not trade; a
market order never rests, so whatever the book could not fill is simply gone.

The case does not read stdin. `run.args` selects the `testapi-array` runner
protocol, whose host supplies strings, booleans, `I32` arithmetic with a single
`leq_i32` comparison, printing, an integer and a boolean formatter, string
concatenation, a recursive loop driver, and mutable arrays. Equality, strict
order, and negation are derived from `leq_i32` in Kio. The package depends on the
`elab` POC for `match!` and `widen_sum!`.

## The event fixture

The arriving flow is a list of nineteen events in the Kio source (`book/event`),
not a stdin fixture, so the transcript stands on its own. It is a
label-generated sum -- `LIMIT(side, price, quantity)`, `MARKET(side, quantity)`,
`CANCEL(id)`, `AMEND(id, quantity)` -- dispatched with `match!`, and it is shaped
so that every path through the engine fires:

- an order that rests untouched, and a second order joining a price level that
  is already occupied, which must queue behind the order that got there first;
- a limit order that fills completely on arrival;
- a limit order that fills partially and rests with the remainder;
- a market order that walks three price levels in one event;
- a market order that exhausts its side of the book and is left unfilled, which
  the transcript reports rather than treating as an error;
- a cancel that removes a resting order, and a cancel that finds nothing because
  the order it names traded two events earlier;
- an amend that reduces an order's quantity, an amend that would grow one and is
  refused, and an amend of an order that was already cancelled;
- a limit order that takes part of a resting order and leaves that maker resting
  with the rest.

An amend may only shrink an order. A reduction keeps the order's place in the
queue; growing it would have to send it to the back of its level, which is a new
order in all but name, so the book refuses and the trader may enter one.

## Reading stdout

The transcript prints one block per event: the event as it arrived, the trades
it generated, and the state of the book it left behind.

```text
event 15: MARKET buy 8 (id 10)
  trade taker 10 maker 2  qty 4 @ 102
  trade taker 10 maker 8  qty 2 @ 103
  trade taker 10 maker 9  qty 2 @ 105
  fill 8 of 8
  book: bid 101  ask 105  spread 4
```

Each order-creating event is given an id in arrival order, shown in its header.
`trade taker T maker M qty Q @ P` is one fill: order `T` was the aggressor,
order `M` the resting order it took from, `Q` units at price `P`. `fill` is how
much of the arriving order traded; `rest` is what it left in the book; `unfilled`
is what a market order could not get. `book:` closes the block with the best bid,
the best ask, and the spread between them; `--` means that side of the book is
empty, and a spread needs both sides to exist.

The resting book then prints by price level, best price first on each side, with
the orders of each level in queue order. `qty` is what the order arrived with and
`rem` is what is still on offer, so a maker that was partially filled and stayed
resting shows up as the two differing. The summary reports the number of trades,
the volume and notional they moved, the orders cancelled, and the market quantity
left unfilled.

The closing invariant is a genuine self-check, not decoration. It counts the
traded volume twice, by two routes that share no arithmetic. The trade tape is
one: add up the quantity of every trade. The book's ledger is the other: it
counts quantity crossing the book's boundary as it happens -- what entered when
an order came to rest, and what left by cancel or by reduce -- and never looks at
the tape. Whatever entered and did not leave by those doors, and is not still
resting, must have left by trading, so the two totals have to agree. If they ever
disagreed, one of them had lost an order.

What this adds to the corpus: the corpus's first matching engine -- an
exchange/market simulation, where the existing array-backed castles are caches,
arenas, and dynamic-programming tables. It exercises price-time priority as an
ordering invariant maintained across insertion and removal (a removal must shift
the queue rather than swap, or the time half of the rule is destroyed),
aggressive orders walking several price levels in a single event, partial fills
that leave a remainder resting and a maker that stays in the queue with what is
left of it, a sum-typed side stored inside records inside a host array, and a
volume invariant checked two independent ways at the end.
