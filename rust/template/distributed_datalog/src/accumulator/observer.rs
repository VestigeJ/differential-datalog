use std::any::Any;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::LinkedList;
use std::fmt::Debug;
use std::hash::Hash;
use std::iter::FromIterator;

use log::trace;
use uid::Id;

use differential_datalog::program::Update;
use differential_datalog::program::RelId;

use crate::Observable;
use crate::ObservableBox;
use crate::Observer;
use crate::ObserverBox;
use crate::OptionalObserver;
use crate::SharedObserver;

/// Wrapper around a `SharedObserver` that inspects the updates to derive the current state.
/// Apart from that it simply forwards all messages to the observer.
#[derive(Debug)]
pub struct AccumulatingObserver<T, V, E>
    where
        V: Debug + Eq + Hash
{
    id: usize,
    /// The observable we track and our subscription to it. TODO: use a better data structure
    subscription: Option<(ObservableBox<T, E>, Box<dyn Any + Send>)>,
    /// The observer we ultimately push our data to.
    observer: SharedObserver<OptionalObserver<ObserverBox<T, E>>>,
    /// The data we accumulated so far.
    data: HashMap<RelId, HashSet<V>>,
    /// Temporary buffer to cache the updates before committing.
    buffer: Option<LinkedList<Vec<T>>>,
}


impl<T, V, E> AccumulatingObserver<T, V, E>
    where
        V: Clone + Debug + Eq + Hash + Send + 'static,
{
    pub fn new() -> Self {
        let id = Id::<()>::new().get();
        trace!("AccumulatingObserver({})::new", id);

        Self {
            id,
            subscription: None,
            observer: SharedObserver::default(),
            data: HashMap::new(),
            buffer: None,
        }
    }

    pub fn get_current_state(&self) -> HashMap<RelId, HashSet<V>> {
        trace!("AccumulatingObserver({})::get_current_state()", self.id);
        // TODO: derive current state and return it
        self.data.clone()
    }
}

impl<T, V, E> Observable<T, E> for AccumulatingObserver<T, V, E>
    where
        T: Debug + Send + 'static,
        V: Debug + Eq + Hash,
        E: Debug + Send + 'static,
{
    type Subscription = ();

    fn subscribe(
        &mut self,
        observer: ObserverBox<T, E>,
    ) -> Result<Self::Subscription, ObserverBox<T, E>> {
        trace!("AccumulatingObserver({})::subscribe()", self.id);
        let mut guard = self.observer.lock().unwrap();
        if guard.is_some() {
            Err(observer)
        } else {
            let _ = guard.replace(observer);
            Ok(())
        }
    }

    fn unsubscribe(&mut self, _subscription: &Self::Subscription) -> Option<ObserverBox<T, E>> {
        trace!("AccumulatingObserver({})::unsubscribe()", self.id);
        self.observer.lock().unwrap().take()
    }
}


/// Forwards the incoming data to the observer while keeping track of the current state
/// TODO: implement accumulating functionality!
impl<V, E> Observer<Update<V>, E> for AccumulatingObserver<Update<V>, V, E>
    where
        V: Debug + Send + Eq + Hash + Clone,
        E: Debug + Send
{
    fn on_start(&mut self) -> Result<(), E> {
        trace!("AccumulatingObserver({})::on_start", self.id);

        if self.buffer.is_some() {
            panic!("received multiple on_start events")
        } else {
            self.buffer = Some(LinkedList::new());
            let mut guard = self.observer.lock().unwrap();
            guard.on_start()
        }
    }

    fn on_commit(&mut self) -> Result<(), E> {
        trace!("AccumulatingObserver({})::on_commit", self.id);

        if let Some(buffer) = self.buffer.take() {
            {
                let mut guard = self.observer.lock().unwrap();
                guard.on_commit()?;
            }
            // apply the buffered updates to the accumulated state
            buffer.into_iter().flatten().for_each(|upd: Update<V>| match upd {
                Update::Insert { relid, v } => {
                    let _ = self.data.entry(relid)
                        .and_modify(|set| { let _ = set.insert(v.clone()); })
                        .or_insert(HashSet::from_iter(vec![v.clone()].into_iter()));
                }
                Update::DeleteValue { relid, v } => {
                    let _ = self.data.entry(relid)
                        .and_modify(|set| { let _ = set.remove(&v); });
                }
                update => panic!("Operation {:?} not allowed", update)
            });

            Ok(())
        } else {
            panic!("on_commit was not preceded by an on_start event")
        }
    }

    fn on_updates<'a>(&mut self, updates: Box<dyn Iterator<Item=Update<V>> + 'a>) -> Result<(), E> {
        trace!("AccumulatingObserver({})::on_updates", self.id);

        if let Some(ref mut buffer) = self.buffer {
            // TODO: copy incoming updates to buffer
            let upds = updates.collect::<Vec<_>>();
            buffer.push_back(upds.clone());
            let mut guard = self.observer.lock().unwrap();
            guard.on_updates(Box::new(upds.into_iter()))
        } else {
            panic!("on_updates was not preceded by an on_start event")
        }
    }


    fn on_completed(&mut self) -> Result<(), E> {
        trace!("AccumulatingObserver({})::on_completed", self.id);
        let mut guard = self.observer.lock().unwrap();
        guard.on_completed()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::sync::Mutex;

    use crate::MockObserver;

    //TODO: test accumulation of data across multiple commits

    /// Test pass-through filter behaviour for transactions via a `AccumulatingObserver`.
    #[test]
    fn transparent_transactions_proxy() {
        let mut observer = AccumulatingObserver::<_, ()>::new();
        let mock = Arc::new(Mutex::new(Some(MockObserver::default())));
        let _subscription = observer.subscribe(Box::new(mock.clone()));

        assert_eq!(observer.on_start(), Ok(()));
        assert_eq!(mock.lock().unwrap().as_ref().unwrap().called_on_start, 1);

        assert_eq!(observer.on_updates(Box::new([1, 3, 2].iter())), Ok(()));
        assert_eq!(mock.lock().unwrap().as_ref().unwrap().called_on_updates, 3);

        assert_eq!(observer.on_updates(Box::new([6, 4, 5].iter())), Ok(()));
        assert_eq!(mock.lock().unwrap().as_ref().unwrap().called_on_updates, 6);

        assert_eq!(observer.on_commit(), Ok(()));
        assert_eq!(mock.lock().unwrap().as_ref().unwrap().called_on_commit, 1);

        assert_eq!(observer.on_start(), Ok(()));
        assert_eq!(mock.lock().unwrap().as_ref().unwrap().called_on_start, 2);

        assert_eq!(observer.on_updates(Box::new([7, 9, 8, 10].iter())), Ok(()));
        assert_eq!(mock.lock().unwrap().as_ref().unwrap().called_on_updates, 10);

        assert_eq!(observer.on_commit(), Ok(()));
        assert_eq!(mock.lock().unwrap().as_ref().unwrap().called_on_commit, 2);

        assert_eq!(observer.on_completed(), Ok(()));
        assert_eq!(mock.lock().unwrap().as_ref().unwrap().called_on_completed, 1);
    }

    /// Test subscribing and unsubscribing for an `AccumulatingObserver`.
    #[test]
    fn subscribe_unsubscribe_observable() {
        let mut observer = AccumulatingObserver::<(), ()>::new();
        let mock = Box::new(MockObserver::new());

        let subscription = observer.subscribe(mock);
        assert!(subscription.is_ok());
        assert!(observer.unsubscribe(&subscription.unwrap()).is_some());
        assert!(observer.observer.lock().unwrap().is_none());
    }

    /// Test multiple subscriptions to an `AccumulatingObserver`.
    #[test]
    fn multiple_subscribe_observable() {
        let mut observer = AccumulatingObserver::<(), ()>::new();
        let mock1 = Box::new(MockObserver::new());
        let mock2 = Box::new(MockObserver::new());

        assert!(observer.subscribe(mock1).is_ok());
        assert!(observer.subscribe(mock2).is_err());
    }
}