//! A doubly linked list that does not owns its nodes.
//!
//! [`List`] safely implements a doubly linked list that does not owns its nodes.
//! The key is that, to access a `Node` that is already inside a `List`, you need to borrow both the `Node` **and the `List`**.
//! * `NodeRef`/`NodeMut` is a pointer to a `Node`, and exists only while the `Node` is inside a `List`.
//! * A `NodeRef` immutably borrows both the node **and the list** for its lifetime.
//! * A `NodeMut` mutably borrows both the node **and the list** for its lifetime.
//! * If a `Node` drops while it is still inside a `List`, we panic. (This is the only runtime cost we have.)
//!
//! In this way, we can safely implement a list without restricting functionality
//! (e.g. disallowing nodes from getting removed from a list once it gets inserted,
//! or disallowing nodes from being dropped before the list drops (even if the node was already removed from the list), etc).
//!
//! Also, note that in this way, we make the `List` logically the *borrow owner* of all of its nodes. That is,
//! * You always need a `ListRef` to immutably access a `Node`.
//! * You always need a `ListMut` to mutably access a `Node`.
//!
//! # List that does not own its nodes
//!
//! In Rust, a list usually owns its nodes. This is the easiest way to guarantee safety,
//! since we can access the elements only through the list's API in this way.
//!
//! However, often, we need a list that does not owns its nodes.
//! For example, the nodes may need to be scattered all around instead of being together in a single array.
//! Or, the nodes may need to be stored not only on the heap, but also on the stack or global data.
//!
//! This module's [`List`] implements such list without sacrificying functionality. Note that
//! * A `Node` can be stored anywhere. (e.g. On the stack, heap, global data, etc.)
//! * A `Node` can drop at any time if it is not inside a `List`.
//! (i.e. A `Node` does not need to statically outlive the `List`, and conversely, the `List` does not need to statically outlive the `Node`s.)

// TODO: Check if `T` is `Unpin`. (Assumed `Unpin` in the following)
// TODO: Add cursor? (std::collections::linked_list::Cursor, CursorMut)

use core::marker::{PhantomData, PhantomPinned};
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::ptr;

use pin_project::{pin_project, pinned_drop};

/// A doubly linked list that does not own the `Node`s.
/// See the module documentation for details.
#[pin_project(PinnedDrop)]
pub struct List<T> {
    #[pin]
    head: ListEntry,
    _marker: PhantomData<T>,
}

/// An immutable reference to a `List`.
/// Grants immutable access to the `List` and any of its `Node`s.
pub struct ListRef<'s, T>(&'s List<T>);

/// A mutable reference to a `List`.
/// Grants unique mutable access to the `List` and any of its `Node`s.
pub struct ListMut<'s, T>(Pin<&'s mut List<T>>);

/// An iterator over the elements of a `List`.
pub struct Iter<'s, T> {
    head: *mut ListEntry, // Use *const or &'s instead?
    tail: *mut ListEntry,
    _marker: PhantomData<&'s List<T>>,
}

/// A mutable iterator over the elements of a `List`.
pub struct IterMut<'s, T> {
    head: *mut ListEntry,
    tail: *mut ListEntry,
    _marker: PhantomData<&'s mut List<T>>,
}

/// A cursor over a `List`.
/// A `Cursor` is like an iterator, except that it can freely seek back-and-forth.
pub struct Cursor<'s, T> {
    head: *mut ListEntry,
    current: *mut ListEntry,
    _marker: PhantomData<&'s List<T>>,
}

/// A cursor over a `List` with editing operations.
/// A `CursorMut` is like an iterator, except that it can freely seek back-and-forth, and can safely mutate the list during iteration.
///
/// Note that unlike `Cursor`, this provides references that borrow the `CursorMut` itself, instead of the `List`.
/// In this way, we can ensure only one mutable reference exists for each `List`.
pub struct CursorMut<'s, T> {
    head: *mut ListEntry,
    current: *mut ListEntry,
    _marker: PhantomData<&'s mut List<T>>,
}

/// A node that can be inserted into a `List`.
/// * To actually read the inner data, you need a `NodeRef` (which needs a `ListRef`).
/// * To actually mutate the inner data or insert/remote this node into/from a `List`, you need a `NodeMut` (which needs a `ListMut`).
/// * Before dropping this `Node`, you must first remove this node from the `List`.
// SAFETY: A `Node` does not drop while a `NodeRef`/`NodeMut` exists. (Uses a single `assert!` to check this)
#[pin_project(PinnedDrop)]
pub struct Node<T> {
    #[pin]
    list_entry: ListEntry,
    data: T,
}

/// A low level primitive for doubly, intrusive linked lists and nodes.
///
/// # Safety
///
/// * All `ListEntry` types must be used only after initializing it with `ListEntry::init`.
/// After this, `ListEntry::{prev, next}` always refer to a valid, initialized `ListEntry`.
#[pin_project]
struct ListEntry {
    prev: *mut Self,
    next: *mut Self,
    #[pin]
    _marker: PhantomPinned, //`ListEntry` is `!Unpin`.
}

/// An immutable reference to a `Node` that is inserted inside a `List`.
/// This type immutably borrows both the `Node` **and the `List`** for `'s`.
// SAFETY: The `Node` is already inserted inside a `List`.
//         `NodeRef` immutably borrows both the 1) `ListRef` and the 2) `Node` for `'s`.
pub struct NodeRef<'s, T>(&'s Node<T>);

/// A mutable reference to a `Node` that is inserted inside a `List`.
/// This type mutably borrows both the `Node` **and the `List`** for `'s`.
// SAFETY: The `Node` is already inserted inside a `List`.
//         `NodeMut` mutably borrows both the 1) `ListMut` and the 2) `Node` for `'s`.
pub struct NodeMut<'s, T>(Pin<&'s mut Node<T>>);

impl<T> List<T> {
    /// Returns a new `List`.
    /// Use `List::get_ref` or `List::get_mut` to do something with the `List`.
    ///
    /// # Safety
    ///
    /// Use after initialization.
    pub unsafe fn new() -> Self {
        Self {
            head: unsafe { ListEntry::new() },
            _marker: PhantomData,
        }
    }

    /// Initializes the `List`.
    pub fn init(self: Pin<&mut Self>) {
        self.project().head.init();
    }

    /// Returns a `ListRef` that points to this `List`.
    pub fn as_list_ref(&self) -> ListRef<'_, T> {
        ListRef(self)
    }

    /// Returns a `ListMut` that points to this `List`.
    #[allow(clippy::wrong_self_convention)]
    pub fn as_list_mut(self: Pin<&mut Self>) -> ListMut<'_, T> {
        ListMut(self)
    }
}

#[pinned_drop]
impl<T> PinnedDrop for List<T> {
    fn drop(mut self: Pin<&mut Self>) {
        while !self.as_list_ref().is_empty() {
            self.as_mut().as_list_mut().pop_front();
        }
    }
}

impl<'s, T> ListRef<'s, T> {
    /// Returns `true` if the `List` is empty.
    pub fn is_empty(&self) -> bool {
        self.0.head.is_unlinked()
    }

    /// Provides a `NodeRef` to the back element, or `None` if the list is empty.
    // SAFETY: `NodeRef` does not actually borrow the `Node` here.
    // However, this is safe since we cannot obtain a `ListMut`
    // (and hence, a `NodeMut`) while a `NodeRef` exists.
    // That is, we cannot mutably access/remove the `Node`.
    // Also, the `Node` does not drop while a `NodeRef` exists.
    pub fn back(&self) -> Option<NodeRef<'_, T>> {
        if self.is_empty() {
            None
        } else {
            let ptr = Node::from_list_entry(self.0.head.prev() as *mut _);
            Some(NodeRef(unsafe { &*ptr }))
        }
    }

    /// Provides a `NodeRef` to the front element, or `None` if the list is empty.
    // SAFETY: `NodeRef` does not actually borrow the `Node` here.
    // However, this is safe since we cannot obtain a `ListMut`
    // (and hence, a `NodeMut`) while a `NodeRef` exists.
    // That is, we cannot mutably access/remove the `Node`.
    // Also, the `Node` does not drop while a `NodeRef` exists.
    pub fn front(&self) -> Option<NodeRef<'_, T>> {
        if self.is_empty() {
            None
        } else {
            let ptr = Node::from_list_entry(self.0.head.next() as *mut _);
            Some(NodeRef(unsafe { &*ptr }))
        }
    }

    /// Provides a forward iterator.
    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            head: self.0.head.next(),
            tail: &self.0.head as *const _ as *mut _,
            _marker: PhantomData,
        }
    }

    /// Provides a cursor at the back element.
    /// The cursor is pointing to the "ghost" non-element if the list is empty.
    pub fn cursor_back(&self) -> Cursor<'_, T> {
        Cursor {
            head: &self.0.head as *const _ as *mut _,
            current: self.0.head.prev(),
            _marker: PhantomData,
        }
    }

    /// Provides a cursor at the front element.
    /// The cursor is pointing to the "ghost" non-element if the list is empty.
    pub fn cursor_front(&self) -> Cursor<'_, T> {
        Cursor {
            head: &self.0.head as *const _ as *mut _,
            current: self.0.head.next(),
            _marker: PhantomData,
        }
    }
}

impl<'s, T> ListMut<'s, T> {
    /// Returns `true` if the `List` is empty.
    pub fn is_empty(&self) -> bool {
        self.0.head.is_unlinked()
    }

    /// Provides a `NodeRef` to the back element, or `None` if the list is empty.
    pub fn back(&self) -> Option<NodeRef<'_, T>> {
        if self.is_empty() {
            None
        } else {
            let ptr = Node::from_list_entry(self.0.head.prev() as *mut _);
            Some(NodeRef(unsafe { &*ptr }))
        }
    }

    /// Provides a `NodeRef` to the front element, or `None` if the list is empty.
    pub fn front(&self) -> Option<NodeRef<'_, T>> {
        if self.is_empty() {
            None
        } else {
            let ptr = Node::from_list_entry(self.0.head.next() as *mut _);
            Some(NodeRef(unsafe { &*ptr }))
        }
    }

    /// Provides a `NodeMut` to the back element, or `None` if the list is empty.
    // SAFETY: `NodeMut` does not actually borrow the `Node` here.
    // However, this is safe since only one `NodeMut` exists for each `List` anyway.
    // Also, the `Node` does not drop while a `NodeMut` exists.
    pub fn back_mut(&mut self) -> Option<NodeMut<'_, T>> {
        if self.is_empty() {
            None
        } else {
            let ptr = Node::from_list_entry(self.0.head.prev() as *mut _);
            Some(NodeMut(unsafe { Pin::new_unchecked(&mut *ptr) }))
        }
    }

    /// Provides a `NodeMut` to the front element, or `None` if the list is empty.
    // SAFETY: `NodeMut` does not actually borrow the `Node` here.
    // However, this is safe since only one `NodeMut` exists for each `List` anyway.
    // Also, the `Node` does not drop while a `NodeMut` exists.
    pub fn front_mut(&mut self) -> Option<NodeMut<'_, T>> {
        if self.is_empty() {
            None
        } else {
            let ptr = Node::from_list_entry(self.0.head.next() as *mut _);
            Some(NodeMut(unsafe { Pin::new_unchecked(&mut *ptr) }))
        }
    }

    /// Appends an element to the back of a list, and returns a `NodeMut` of it.
    // SAFETY: `NodeMut` does not actually borrow the `Node` here.
    // However, this is safe since only one `NodeMut` exists for each `List` anyway.
    // Also, the `Node` does not drop while a `NodeMut` exists.
    pub fn push_back<'t>(&'t mut self, mut node: Pin<&'t mut Node<T>>) -> NodeMut<'t, T> {
        self.0
            .as_mut()
            .project()
            .head
            .push_back(node.as_mut().project().list_entry);
        NodeMut(node)
    }

    /// Appends an element to the front of a list, and returns a `NodeMut` of it.
    // SAFETY: `NodeMut` does not actually borrow the `Node` here.
    // However, this is safe since only one `NodeMut` exists for each `List` anyway.
    // Also, the `Node` does not drop while a `NodeMut` exists.
    pub fn push_front<'t>(&'t mut self, mut node: Pin<&'t mut Node<T>>) -> NodeMut<'t, T> {
        self.0
            .as_mut()
            .project()
            .head
            .push_front(node.as_mut().project().list_entry);
        NodeMut(node)
    }

    /// Removes the last element from a list.
    pub fn pop_back(&mut self) {
        if let Some(node_mut) = self.back_mut() {
            node_mut.remove();
        }
    }

    /// Removes the first element from a list.
    pub fn pop_front(&mut self) {
        if let Some(node_mut) = self.front_mut() {
            node_mut.remove();
        }
    }

    /// Provides a forward iterator.
    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            head: self.0.head.next(),
            tail: &self.0.head as *const _ as *mut _,
            _marker: PhantomData,
        }
    }

    /// Provides a forward iterator with mutable references.
    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        IterMut {
            head: self.0.head.next(),
            tail: &self.0.head as *const _ as *mut _,
            _marker: PhantomData,
        }
    }

    /// Provides a cursor with editing operations at the back element.
    /// The cursor is pointing to the "ghost" non-element if the list is empty.
    pub fn cursor_back_mut(&mut self) -> CursorMut<'_, T> {
        CursorMut {
            head: &self.0.head as *const _ as *mut _,
            current: self.0.head.prev(),
            _marker: PhantomData,
        }
    }

    /// Provides a cursor with editing operations at the front element.
    /// The cursor is pointing to the "ghost" non-element if the list is empty.
    pub fn cursor_front_mut(&mut self) -> CursorMut<'_, T> {
        CursorMut {
            head: &self.0.head as *const _ as *mut _,
            current: self.0.head.next(),
            _marker: PhantomData,
        }
    }
}

impl<'s, T> Iterator for Iter<'s, T> {
    type Item = &'s T;

    fn next(&mut self) -> Option<Self::Item> {
        if ptr::eq(self.head, self.tail) {
            None
        } else {
            // Safe since `self.head` is a `ListEntry` contained inside a `T`.
            let node: &Node<T> = unsafe { &*Node::from_list_entry(self.head) };
            self.head = node.list_entry.next();
            Some(&node.data)
        }
    }
}

impl<'s, T> DoubleEndedIterator for Iter<'s, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if ptr::eq(self.head, self.tail) {
            None
        } else {
            self.tail = unsafe { &*self.tail }.prev();
            // Safe since `self.last` is a `ListEntry` contained inside a `T`.
            Some(&unsafe { &*Node::from_list_entry(self.tail) }.data)
        }
    }
}

impl<'s, T> Iterator for IterMut<'s, T> {
    type Item = &'s mut T;

    fn next(&mut self) -> Option<Self::Item> {
        if ptr::eq(self.head, self.tail) {
            None
        } else {
            // Safe since `self.head` is a `ListEntry` contained inside a `T`.
            let node: &mut Node<T> = unsafe { &mut *Node::from_list_entry(self.head) };
            self.head = node.list_entry.next();
            Some(&mut node.data)
        }
    }
}

impl<'s, T> DoubleEndedIterator for IterMut<'s, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if ptr::eq(self.head, self.tail) {
            None
        } else {
            self.tail = unsafe { &*self.tail }.prev();
            // Safe since `self.last` is a `ListEntry` contained inside a `T`.
            Some(&mut unsafe { &mut *Node::from_list_entry(self.tail) }.data)
        }
    }
}

impl<'s, T> Cursor<'s, T> {
    pub fn current(&self) -> Option<&'s T> {
        if ptr::eq(self.head, self.current) {
            None
        } else {
            Some(unsafe { &*Node::from_list_entry(self.current) }.data)
        }
    }

    pub fn move_prev(&mut self) {
        self.current = unsafe { &*self.current }.prev();
    }

    pub fn move_next(&mut self) {
        self.current = unsafe { &*self.current }.next();
    }

    pub fn peek_prev(&self) -> Option<&'s T> {
        let ptr = unsafe { &*self.current }.prev();
        if ptr::eq(self.head, ptr) {
            None
        } else {
            Some(unsafe { &*Node::from_list_entry(ptr) }.data)
        }
    }

    pub fn peek_next(&self) -> Option<&'s T> {
        let ptr = unsafe { &*self.current }.next();
        if ptr::eq(self.head, ptr) {
            None
        } else {
            Some(unsafe { &*Node::from_list_entry(ptr) }.data)
        }
    }
}

impl<'s, T> CursorMut<'s, T> {
    fn current_entry(&mut self) -> Pin<&mut ListEntry> {
        unsafe { Pin::new_unchecked(&mut *self.current) }
    }

    pub fn current(&mut self) -> Option<&mut T> {
        if ptr::eq(self.head, self.current) {
            None
        } else {
            Some(&mut unsafe { &mut *Node::from_list_entry(self.current) }.data)
        }
    }

    pub fn move_prev(&mut self) {
        self.current = unsafe { &*self.current }.prev();
    }

    pub fn move_next(&mut self) {
        self.current = unsafe { &*self.current }.next();
    }

    pub fn peek_prev(&mut self) -> Option<&mut T> {
        let ptr = unsafe { &*self.current }.prev();
        if ptr::eq(self.head, ptr) {
            None
        } else {
            Some(&mut unsafe { &mut *Node::from_list_entry(ptr) }.data)
        }
    }

    pub fn peek_next(&mut self) -> Option<&mut T> {
        let ptr = unsafe { &*self.current }.next();
        if ptr::eq(self.head, ptr) {
            None
        } else {
            Some(&mut unsafe { &mut *Node::from_list_entry(ptr) }.data)
        }
    }

    pub fn insert_before<'t>(&'t mut self, mut node: Pin<&'t mut Node<T>>) -> &'t mut T {
        self.current_entry()
            .push_back(node.as_mut().project().list_entry);
        node.project().data
    }

    pub fn insert_after<'t>(&'t mut self, mut node: Pin<&'t mut Node<T>>) -> &'t mut T {
        self.current_entry()
            .push_front(node.as_mut().project().list_entry);
        node.project().data
    }

    pub fn remove_current(&mut self) {
        if !ptr::eq(self.head, self.current) {
            let entry = self.current_entry();
            let ptr = entry.next();
            entry.remove();
            self.current = ptr;
        }
    }
}

impl<T> Node<T> {
    const LIST_ENTRY_OFFSET: usize = 0;

    // TODO: use `offset_of!` instead.

    /// Returns a new `Node`.
    ///
    /// # Safety
    ///
    /// Use after initialization.
    pub unsafe fn new(data: T) -> Self {
        Self {
            list_entry: unsafe { ListEntry::new() },
            data,
        }
    }

    /// Initializes the `Node`.
    pub fn init(self: Pin<&mut Self>) {
        self.project().list_entry.init();
    }

    /// Returns an immutable reference to the inner data if the `Node` is not inside a `List`.
    /// Otherwise, returns `None`.
    pub fn get_inner(&self) -> Option<&T> {
        if self.list_entry.is_unlinked() {
            Some(&self.data)
        } else {
            None
        }
    }

    /// Returns a mutable reference to the inner data if the `Node` is not inside a `List`.
    /// Otherwise, returns `None`.
    pub fn get_mut_inner(self: Pin<&mut Self>) -> Option<&mut T> {
        if self.list_entry.is_unlinked() {
            Some(&mut unsafe { self.get_unchecked_mut() }.data)
        } else {
            None
        }
    }

    /// Returns a `NodeRef` that points to this `Node`.
    /// The `NodeRef` immutably borrows the `Node` **and the `List`** for its lifetime.
    ///
    /// # Safety
    ///
    /// The `Node` must already be inserted inside the list.
    pub unsafe fn as_ref_unchecked<'s>(&'s self, _list_ref: &'s ListRef<'_, T>) -> NodeRef<'s, T> {
        NodeRef(self)
    }

    /// Returns a `NodeMut` that points to this `Node`.
    /// The `NodeRef` mutably borrows the `Node` **and the `List`** for its lifetime.
    ///
    /// # Safety
    ///
    /// The `Node` must already be inserted inside the list.
    #[allow(clippy::wrong_self_convention)]
    pub unsafe fn as_mut_unchecked<'s>(
        self: Pin<&'s mut Self>,
        _list_mut: &'s mut ListMut<'_, T>,
    ) -> NodeMut<'s, T> {
        NodeMut(self)
    }

    /// Converts a raw pointer of a `ListEntry` into a raw pointer of the `Node` that owns the `ListEntry`.
    fn from_list_entry(list_entry: *mut ListEntry) -> *mut Self {
        (list_entry as usize - Self::LIST_ENTRY_OFFSET) as *mut Self
    }
}

#[pinned_drop]
impl<T> PinnedDrop for Node<T> {
    fn drop(self: Pin<&mut Self>) {
        // A `Node` should not drop while its inside a `List`. It should always be removed first.
        // Note that we can't do this implicitly, since the `drop` function only takes 1 argument.
        assert!(
            self.project().list_entry.is_unlinked(),
            "Node dropped while its inside a list"
        );
    }
}

impl<T> Deref for NodeRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.data
    }
}

impl<'s, T> NodeMut<'s, T> {
    /// Removes the `Node` from the `List`.
    pub fn remove(self) {
        self.0.project().list_entry.remove();
    }
}

impl<T> Deref for NodeMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.data
    }
}

impl<T> DerefMut for NodeMut<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().project().data
    }
}

impl ListEntry {
    /// Returns an uninitialized `ListEntry`,
    ///
    /// # Safety
    ///
    /// All `ListEntry` types must be used only after initializing it with `ListEntry::init`.
    const unsafe fn new() -> Self {
        Self {
            prev: ptr::null_mut(),
            next: ptr::null_mut(),
            _marker: PhantomPinned,
        }
    }

    /// Initializes this `ListEntry` if it was not initialized.
    /// Otherwise, does nothing.
    fn init(mut self: Pin<&mut Self>) {
        if self.next().is_null() {
            let ptr = unsafe { self.as_mut().get_unchecked_mut() } as *mut Self;
            *self.as_mut().project().prev = ptr;
            *self.as_mut().project().next = ptr;
        }
    }

    /// Returns a raw pointer pointing to the previous `ListEntry`.
    ///
    /// # Note
    ///
    /// Do not use `ListNode::from_list_entry` on the returned pointer if `self` is the front node of a list.
    fn prev(&self) -> *mut Self {
        self.prev
    }

    /// Returns a raw pointer pointing to the next `ListEntry`.
    ///
    /// # Note
    ///
    /// Do not use `ListNode::from_list_entry` on the returned pointer if `self` is the back node of a list.
    fn next(&self) -> *mut Self {
        self.next
    }

    /// Returns `true` if this `ListEntry` is not linked to any other `ListEntry`.
    /// Otherwise, returns `false`.
    fn is_unlinked(&self) -> bool {
        ptr::eq(self.next(), self)
    }

    /// Inserts `elt` at the back of this `ListEntry` after unlinking `elt`.
    fn push_back(mut self: Pin<&mut Self>, mut elt: Pin<&mut Self>) {
        if !elt.is_unlinked() {
            elt.as_mut().remove();
        }

        let s = unsafe { self.as_mut().get_unchecked_mut() };
        let e = unsafe { elt.as_mut().get_unchecked_mut() };

        e.prev = s.prev();
        e.next = s;
        unsafe {
            (*e.next()).prev = e;
            (*e.prev()).next = e;
        }
    }

    /// Inserts `elt` in front of this `ListEntry` after unlinking `elt`.
    fn push_front(mut self: Pin<&mut Self>, mut elt: Pin<&mut Self>) {
        if !elt.is_unlinked() {
            elt.as_mut().remove();
        }

        let s = unsafe { self.as_mut().get_unchecked_mut() };
        let e = unsafe { elt.as_mut().get_unchecked_mut() };

        e.prev = s;
        e.next = self.next();
        unsafe {
            (*e.next()).prev = e;
            (*e.prev()).next = e;
        }
    }

    /// Unlinks this `ListEntry` from other `ListEntry`s.
    fn remove(mut self: Pin<&mut Self>) {
        let s = unsafe { self.as_mut().get_unchecked_mut() };

        unsafe {
            (*s.prev()).next = s.next();
            (*s.next()).prev = s.prev();
        }
        s.prev = s;
        s.next = s;
    }
}

pub fn test() {
    // Create `List` and `Nodes`. Pin them on the stack.
    let mut list = unsafe { List::new() };
    let mut list = unsafe { Pin::new_unchecked(&mut list) };
    let mut node1 = unsafe { Node::new(10) };
    let mut node1 = unsafe { Pin::new_unchecked(&mut node1) };
    let mut node2 = unsafe { Node::new(20) };
    let mut node2 = unsafe { Pin::new_unchecked(&mut node2) };

    // Initialize.
    list.as_mut().init();
    node1.as_mut().init();
    node2.as_mut().init();

    // Do something with `ListMut`.
    let mut list_mut = list.as_mut().as_list_mut();
    let _ = list_mut.push_front(node1.as_mut());
    let _ = list_mut.push_back(node2.as_mut());

    assert!(node1.as_mut().get_inner() == None);
    assert!(node2.as_mut().get_mut_inner() == None);

    assert!(*list_mut.front().expect("") == 10);
    assert!(*list_mut.back_mut().expect("") == 20);

    let mut count = 0;
    let mut i = 0;
    for e in list_mut.iter() {
        count += 1;
        i = *e;
    }
    assert!(count == 2);
    assert!(i == 20);

    for e in list_mut.iter_mut() {
        *e += 1;
    }

    let mut node2_mut = list_mut.back_mut().expect("");
    *node2_mut += 10;

    list_mut.pop_back();
    assert!(*node2.get_inner().expect("") == 31);

    // Do something with `ListRef`.
    let list_ref = list.as_list_ref();
    assert!(*list_ref.front().expect("") == *list_ref.back().expect(""));
    for e in list_ref.iter() {
        assert!(*e == 11);
    }

    // Empty the list.
    let mut list_mut = list.as_mut().as_list_mut();
    list_mut.pop_front();
}
