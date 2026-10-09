#![forbid(unsafe_code)]

use rust_recursive_newtype_boundary::host::RustRecursiveNewtypeBoundaryHost;
use rust_recursive_newtype_boundary::shapes::{
    KioNewtypeMarker_recursiveBoundary__Forest, KioNewtypeMarker_recursiveBoundary__Tree, KioValue,
    Product, Sum,
};

#[derive(Clone)]
struct Host;

impl RustRecursiveNewtypeBoundaryHost for Host {
    type __kio_host_recursiveBoundary__I32 = i32;
}

type Tree = rust_recursive_newtype_boundary::shapes::nominal::recursiveBoundary::Tree<Host>;
type Forest = rust_recursive_newtype_boundary::shapes::nominal::recursiveBoundary::Forest<Host>;

fn tree_facade(value: Tree) -> Tree {
    KioValue::<KioNewtypeMarker_recursiveBoundary__Tree<Host>>::pack(value).unpack()
}

fn forest_facade(value: Forest) -> Forest {
    KioValue::<KioNewtypeMarker_recursiveBoundary__Forest<Host>>::pack(value).unpack()
}

fn inspect_forest(value: Forest, expected: i32) {
    let payload: Sum<(), Product<Tree, Forest>> = Forest::unForest(value);
    let Sum::Right(Product {
        _0: child,
        _1: tail,
    }) = payload
    else {
        panic!("expected a nonempty forest");
    };
    let child_payload: Sum<i32, Forest> = Tree::unTree(child);
    assert!(matches!(child_payload, Sum::Left(value) if value == expected));
    let tail_payload: Sum<(), Product<Tree, Forest>> = Forest::unForest(tail);
    assert!(matches!(tail_payload, Sum::Left(())));
}

fn main() {
    let package = rust_recursive_newtype_boundary::create_rustRecursiveNewtypeBoundary(Host);
    let leaf: Tree = tree_facade(package.recursiveBoundary.leaf(41));
    let empty: Forest = forest_facade(package.recursiveBoundary.empty());
    assert!(matches!(Forest::unForest(empty.clone()), Sum::Left(())));
    let forest: Forest = package.recursiveBoundary.graft(leaf, empty);
    let node: Tree = package.recursiveBoundary.node(forest);
    let payload: Sum<i32, Forest> = Tree::unTree(node);
    let Sum::Right(forest) = payload else {
        panic!("expected a forest node");
    };
    inspect_forest(forest_facade(forest), 41);

    let child: Tree = tree_facade(Tree::mkTree(Sum::Left(7)));
    let tail: Forest = forest_facade(Forest::mkForest(Sum::Left(())));
    let constructed: Forest = Forest::mkForest(Sum::Right(Product {
        _0: child,
        _1: tail,
    }));
    inspect_forest(constructed, 7);
}
