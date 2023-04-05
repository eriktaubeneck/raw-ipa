#![allow(dead_code)] // TODO: remove once migrated to new transports
use crate::{
    helpers::{
        query::QueryConfig, HelperIdentity, NoResourceIdentifier, QueryIdBinding, RouteId,
        RouteParams, StepBinding, Transport,
    },
    protocol::{QueryId, Step},
};
use ::tokio::sync::mpsc::{channel, Receiver, Sender};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use futures_util::stream;
use serde::de::DeserializeOwned;
use std::{
    borrow::Borrow,
    collections::{hash_map::Entry, HashMap, HashSet},
    convert,
    fmt::{Debug, Formatter},
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll, Waker},
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::Instrument;

use crate::{
    helpers::{query::PrepareQuery, TransportError},
};
#[cfg(all(feature = "shuttle", test))]
use shuttle::future as tokio;
use tokio::sync::oneshot;
use crate::helpers::TransportCallbacks;


type Packet = (Addr, InMemoryStream, oneshot::Sender<Result<(), TransportError>>);
type ConnectionTx = Sender<Packet>;
type ConnectionRx = Receiver<Packet>;
type StreamItem = Vec<u8>;

/// In-memory implementation of [`Transport`] backed by Tokio mpsc channels.
/// Use [`Setup`] to initialize it and call [`Setup::start`] to make it actively listen for
/// incoming messages.
pub struct InMemoryTransport {
    identity: HelperIdentity,
    connections: HashMap<HelperIdentity, ConnectionTx>,
    record_streams: StreamCollection<InMemoryStream>,
}

impl InMemoryTransport {
    #[must_use]
    fn new(identity: HelperIdentity, connections: HashMap<HelperIdentity, ConnectionTx>) -> Self {
        Self {
            identity,
            connections,
            record_streams: StreamCollection::default(),
        }
    }

    #[must_use]
    pub fn identity(&self) -> HelperIdentity {
        self.identity
    }

    /// TODO: maybe it shouldn't be active, but rather expose a method that takes the next message
    /// out and processes it, the same way as query processor does. That will allow all tasks to be
    /// created in one place (driver). It does not affect the [`Transport`] interface,
    /// so I'll leave it as is for now.
    fn listen(&self, mut callbacks: TransportCallbacks<'static, Weak<Self>>, mut rx: ConnectionRx) {
        tokio::spawn(
            {
                let streams = self.record_streams.clone();
                let this = Arc::downgrade(&self);
                async move {
                    let mut active_queries = HashSet::new();
                    while let Some((addr, stream, ack)) = rx.recv().await {
                        tracing::trace!("received new message: {addr:?}");

                        let result = match addr.route {
                            RouteId::ReceiveQuery => {
                                let qc = addr.into::<QueryConfig>();
                                (callbacks.receive_query)(this.clone(), qc)
                                    .await
                                    .map(|query_id| {
                                        assert!(
                                            active_queries.insert(query_id),
                                            "the same query id {query_id:?} is generated twice"
                                        );
                                    })
                            }
                            RouteId::Records => {
                                let query_id = addr.query_id.unwrap();
                                let step = addr.step.unwrap();
                                let from = addr.origin.unwrap();
                                streams.add_stream((query_id, from, step), stream);
                                Ok(())
                            }
                            RouteId::PrepareQuery => {
                                let input = addr.into::<PrepareQuery>();
                                (callbacks.prepare_query)(this.clone(), input).await
                            }
                        };

                        ack.send(result).unwrap()
                    }
                }
            }
            .instrument(tracing::info_span!("transport_loop", id=?self.identity).or_current()),
        );
    }

    fn get_channel(&self, dest: HelperIdentity) -> ConnectionTx {
        self.connections
            .get(&dest)
            .unwrap_or_else(|| {
                panic!(
                    "Should have an active connection from {:?} to {:?}",
                    self.identity, dest
                );
            })
            .clone()
    }
}

#[async_trait]
impl Transport for Weak<InMemoryTransport> {
    type RecordsStream = ReceiveRecords<InMemoryStream>;

    fn identity(&self) -> HelperIdentity {
        self.upgrade().unwrap().identity
    }

    async fn send<
        D: Stream<Item = Vec<u8>> + Send + 'static,
        Q: QueryIdBinding,
        S: StepBinding,
        R: RouteParams<RouteId, Q, S>,
    >(
        &self,
        dest: HelperIdentity,
        route: R,
        data: D,
    ) -> Result<(), TransportError>
    where
        Option<QueryId>: From<Q>,
        Option<Step>: From<S>,
    {
        let this = self.upgrade().unwrap();
        let channel = this.get_channel(dest);
        let addr = Addr::from_route(this.identity, &route);
        let (ack_tx, ack_rx) = oneshot::channel();

        channel
            .send((addr, InMemoryStream::wrap(data), ack_tx))
            .await
            .map_err(|_e| {
                io::Error::new::<String>(io::ErrorKind::ConnectionAborted, "channel closed".into())
            })?;

        ack_rx
            .await
            .map_err(|_recv_error| TransportError::Rejected {
                dest,
                inner: "channel closed".into(),
            })
            .and_then(convert::identity)
    }

    fn receive<R: RouteParams<NoResourceIdentifier, QueryId, Step>>(
        &self,
        from: HelperIdentity,
        route: R,
    ) -> Self::RecordsStream {
        ReceiveRecords::new(
            (route.query_id(), from, route.step()),
            self.upgrade().unwrap().record_streams.clone(),
        )
    }
}

/// Represents a stream of records.
/// If stream is not received yet, each poll generates a waker that is used internally to wake up
/// the task when stream is received.
/// Once stream is received, it is moved to this struct and it acts as a proxy to it.
pub struct ReceiveRecords<S> {
    inner: ReceiveRecordsInner<S>,
}

impl<S> ReceiveRecords<S> {
    fn new(key: StreamKey, coll: StreamCollection<S>) -> Self {
        Self {
            inner: ReceiveRecordsInner::Pending(key, coll),
        }
    }
}

impl<S: Stream + Unpin> Stream for ReceiveRecords<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::get_mut(self).inner.poll_next_unpin(cx)
    }
}

/// Convenience struct to support heterogeneous in-memory streams
pub struct InMemoryStream {
    /// There is only one reason for this to have dynamic dispatch: tests that use from_iter method.
    inner: Pin<Box<dyn Stream<Item = StreamItem> + Send>>,
}

impl InMemoryStream {
    fn empty() -> Self {
        Self::from_iter(std::iter::empty())
    }

    fn wrap<S: Stream<Item = StreamItem> + Send + 'static>(value: S) -> Self {
        Self {
            inner: Box::pin(value),
        }
    }

    fn from_iter<I>(input: I) -> Self
    where
        I: IntoIterator<Item = StreamItem>,
        I::IntoIter: Send + 'static,
    {
        Self {
            inner: Box::pin(stream::iter(input.into_iter())),
        }
    }
}

impl From<Receiver<StreamItem>> for InMemoryStream {
    fn from(value: Receiver<StreamItem>) -> Self {
        Self {
            inner: Box::pin(ReceiverStream::new(value)),
        }
    }
}

impl Stream for InMemoryStream {
    type Item = StreamItem;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = Pin::get_mut(self);
        this.inner.poll_next_unpin(cx)
    }
}

impl Debug for InMemoryStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "InMemoryStream")
    }
}

struct Addr {
    route: RouteId,
    origin: Option<HelperIdentity>,
    query_id: Option<QueryId>,
    step: Option<Step>,
    params: String,
}

impl Addr {
    fn from_route<Q: QueryIdBinding, S: StepBinding, R: RouteParams<RouteId, Q, S>>(
        origin: HelperIdentity,
        route: &R,
    ) -> Self
    where
        Option<QueryId>: From<Q>,
        Option<Step>: From<S>,
    {
        Self {
            route: route.resource_identifier(),
            origin: Some(origin),
            query_id: route.query_id().into(),
            step: route.step().into(),
            params: route.extra().borrow().to_string(),
        }
    }

    fn into<T: DeserializeOwned>(self) -> T {
        serde_json::from_str(&self.params).unwrap()
    }

    fn receive_query(config: QueryConfig) -> Self {
        Self {
            route: RouteId::ReceiveQuery,
            origin: None,
            query_id: None,
            step: None,
            params: serde_json::to_string(&config).unwrap(),
        }
    }

    fn records(from: HelperIdentity, query_id: QueryId, step: Step) -> Self {
        Self {
            route: RouteId::Records,
            origin: Some(from),
            query_id: Some(query_id),
            step: Some(step),
            params: String::new(),
        }
    }
}

impl Debug for Addr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Addr[route={:?}, query_id={:?}, step={:?}, params={}]",
            self.route, self.query_id, self.step, self.params
        )
    }
}

/// Each stream is indexed by query id, the identity of helper where stream is originated from
/// and step.
type StreamKey = (QueryId, HelperIdentity, Step);

/// Thread-safe append-only collection of homogeneous record streams.
/// Streams are indexed by [`StreamKey`] and the lifecycle of each stream is described by the
/// [`RecordsStream`] struct.
///
/// Each stream can be inserted and taken away exactly once, any deviation from this behaviour will
/// result in panic.
struct StreamCollection<S> {
    inner: Arc<Mutex<HashMap<StreamKey, RecordsStream<S>>>>,
}

impl<S> Default for StreamCollection<S> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::default())),
        }
    }
}

impl<S> Clone for StreamCollection<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S: Stream> StreamCollection<S> {
    /// Adds a new stream associated with the given key.
    ///
    /// ## Panics
    /// If there was another stream associated with the same key some time in the past.
    pub fn add_stream(&self, key: StreamKey, stream: S) {
        let mut streams = self.inner.lock().unwrap();
        match streams.entry(key) {
            Entry::Occupied(mut entry) => match entry.get_mut() {
                rs @ RecordsStream::Waiting(_) => {
                    let RecordsStream::Waiting(waker) = std::mem::replace(rs, RecordsStream::Ready(stream)) else {
                            unreachable!()
                        };
                    waker.wake();
                }
                rs @ (RecordsStream::Ready(_) | RecordsStream::Completed) => {
                    let state = format!("{rs:?}");
                    let key = entry.key().clone();
                    drop(streams);
                    panic!("{key:?} entry state expected to be waiting, got {state:?}");
                }
            },
            Entry::Vacant(entry) => {
                entry.insert(RecordsStream::Ready(stream));
            }
        }
    }

    /// Adds a new waker to notify when the stream is ready. If stream is ready, this method takes
    /// it out, leaving a tombstone in its place, and returns it.
    ///
    /// ## Panics
    /// If [`Waker`] that exists already inside this collection will not wake the given one.
    pub fn add_waker(&self, key: &StreamKey, waker: &Waker) -> Option<S> {
        let mut streams = self.inner.lock().unwrap();

        match streams.entry(key.clone()) {
            Entry::Occupied(mut entry) => {
                match entry.get_mut() {
                    RecordsStream::Waiting(old_waker) => {
                        let will_wake = old_waker.will_wake(waker);
                        drop(streams); // avoid mutex poisoning
                        assert!(will_wake);
                        None
                    }
                    rs @ RecordsStream::Ready(_) => {
                        let RecordsStream::Ready(stream) = std::mem::replace(rs, RecordsStream::Completed) else {
                            unreachable!();
                        };

                        Some(stream)
                    }
                    RecordsStream::Completed => {
                        drop(streams);
                        panic!("{key:?} stream has been consumed already")
                    }
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(RecordsStream::Waiting(waker.clone()));
                None
            }
        }
    }
}

/// Describes the lifecycle of records stream inside [`StreamCollection`]
enum RecordsStream<S> {
    /// There was a request to receive this stream, but it hasn't arrived yet
    Waiting(Waker),
    /// Stream is ready to be consumed
    Ready(S),
    /// Stream was successfully received and taken away from [`StreamCollection`].
    /// It may not be requested or received again.
    Completed,
}

impl<S> Debug for RecordsStream<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordsStream::Waiting(_) => {
                write!(f, "Waiting")
            }
            RecordsStream::Ready(_) => {
                write!(f, "Ready")
            }
            RecordsStream::Completed => {
                write!(f, "Completed")
            }
        }
    }
}

/// Inner state for [`ReceiveRecords`] struct
enum ReceiveRecordsInner<S> {
    Pending(StreamKey, StreamCollection<S>),
    Ready(S),
}

impl<S: Stream + Unpin> Stream for ReceiveRecordsInner<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = Pin::get_mut(self);
        loop {
            match this {
                Self::Pending(key, streams) => {
                    if let Some(stream) = streams.add_waker(key, cx.waker()) {
                        *this = Self::Ready(stream);
                    } else {
                        return Poll::Pending;
                    }
                }
                Self::Ready(stream) => return stream.poll_next_unpin(cx),
            }
        }
    }
}

pub struct Setup {
    identity: HelperIdentity,
    tx: ConnectionTx,
    rx: ConnectionRx,
    callbacks: TransportCallbacks,
    connections: HashMap<HelperIdentity, ConnectionTx>,
}

impl Setup {
    pub fn new(identity: HelperIdentity, callbacks: TransportCallbacks) -> Self {
        let (tx, rx) = channel(16);
        Self {
            identity,
            tx,
            rx,
            connections: HashMap::default(),
        }
    }

    pub fn connect(&mut self, other: &mut Self) {
        assert!(self
            .connections
            .insert(other.identity, other.tx.clone())
            .is_none());
        assert!(other
            .connections
            .insert(self.identity, self.tx.clone())
            .is_none());
    }

    fn into_active_conn(
        self,
        callbacks: TransportCallbacks<'static, Weak<InMemoryTransport>>,
    ) -> (ConnectionTx, Arc<InMemoryTransport>) {
        let transport = Arc::new(InMemoryTransport::new(self.identity, self.connections));
        transport.listen(callbacks, self.rx);

        (self.tx, transport)
    }

    pub fn start(
        self,
        callbacks: TransportCallbacks<'static, Weak<InMemoryTransport>>,
    ) -> Arc<InMemoryTransport> {
        self.into_active_conn(callbacks).1
    }
}

#[cfg(all(test, not(feature = "shuttle")))]
mod tests {
    use super::*;
    use crate::{
        ff::{FieldType, Fp31},
        helpers::{OrderingSender, query::QueryType, HelperIdentity},
        protocol::Step,
        test_fixture::network::InMemoryNetwork,
    };
    use futures_util::{stream::poll_immediate, FutureExt, StreamExt};
    use std::{num::NonZeroUsize, panic::AssertUnwindSafe};
    use std::io::ErrorKind;
    use tokio::sync::{mpsc::channel, oneshot};
    use crate::error::Error;

    const STEP: &str = "in-memory-transport";

    async fn send_and_ack(sender: &ConnectionTx, addr: Addr, data: InMemoryStream) {
        let (tx, rx) = oneshot::channel();
        sender.send((addr, data, tx)).await.unwrap();
        rx.await
            .map_err(|e| TransportError::Io { inner: io::Error::new(ErrorKind::ConnectionRefused, "channel closed" )})
            .and_then(convert::identity).unwrap();
    }

    #[tokio::test]
    async fn callback_is_called() {
        let (signal_tx, signal_rx) = oneshot::channel();
        let signal_tx = Arc::new(Mutex::new(Some(signal_tx)));
        let (tx, _transport) =
            Setup::new(HelperIdentity::ONE).into_active_conn(TransportCallbacks {
                receive_query: Box::new(move |transport, query_config| {
                    let signal_tx = Arc::clone(&signal_tx);
                    Box::pin(async move {
                        // this works because callback is only called once
                        signal_tx
                            .lock()
                            .unwrap()
                            .take()
                            .expect("query callback invoked more than once")
                            .send(query_config)
                            .unwrap();
                        Ok(QueryId)
                    })
                }),
                ..Default::default()
            });
        let expected = QueryConfig {
            field_type: FieldType::Fp32BitPrime,
            query_type: QueryType::TestMultiply,
        };

        send_and_ack(&tx, Addr::receive_query(expected), InMemoryStream::empty()).await;

        assert_eq!(expected, signal_rx.await.unwrap());
    }

    #[tokio::test]
    async fn receive_not_ready() {
        let (tx, transport) =
            Setup::new(HelperIdentity::ONE).into_active_conn(TransportCallbacks::default());
        let transport = Arc::downgrade(&transport);
        let expected = vec![vec![1], vec![2]];

        let mut stream = transport.receive(HelperIdentity::TWO, (QueryId, Step::from(STEP)));

        // make sure it is not ready as it hasn't received the records stream yet.
        assert!(matches!(
            poll_immediate(&mut stream).next().await,
            Some(Poll::Pending)
        ));
        send_and_ack(&tx,
            Addr::records(HelperIdentity::TWO, QueryId, Step::from(STEP)),
            InMemoryStream::from_iter(expected.clone()),
        )
        .await;

        assert_eq!(expected, stream.collect::<Vec<_>>().await);
    }

    #[tokio::test]
    async fn receive_ready() {
        let (tx, transport) =
            Setup::new(HelperIdentity::ONE).into_active_conn(TransportCallbacks::default());
        let expected = vec![vec![1], vec![2]];

        send_and_ack(&tx,
            Addr::records(HelperIdentity::TWO, QueryId, Step::from(STEP)),
            InMemoryStream::from_iter(expected.clone()),
        )
        .await;

        let stream =
            Arc::downgrade(&transport).receive(HelperIdentity::TWO, (QueryId, Step::from(STEP)));

        assert_eq!(expected, stream.collect::<Vec<_>>().await);
    }

    #[tokio::test]
    async fn two_helpers() {
        async fn send_and_verify(
            from: HelperIdentity,
            to: HelperIdentity,
            transports: &HashMap<HelperIdentity, Weak<InMemoryTransport>>,
        ) {
            let (stream_tx, stream_rx) = channel(1);
            let stream = InMemoryStream::from(stream_rx);

            let from_transport = transports.get(&from).unwrap();
            let to_transport = transports.get(&to).unwrap();
            let step = Step::from(STEP);

            let mut recv = to_transport.receive(from, (QueryId, step.clone()));
            assert!(matches!(
                poll_immediate(&mut recv).next().await,
                Some(Poll::Pending)
            ));

            from_transport
                .send(to, (RouteId::Records, QueryId, step.clone()), stream)
                .await
                .unwrap();
            stream_tx.send(vec![1, 2, 3]).await.unwrap();
            assert_eq!(vec![1, 2, 3], recv.next().await.unwrap());
            assert!(matches!(
                poll_immediate(&mut recv).next().await,
                Some(Poll::Pending)
            ));

            stream_tx.send(vec![4, 5, 6]).await.unwrap();
            assert_eq!(vec![4, 5, 6], recv.next().await.unwrap());
            assert!(matches!(
                poll_immediate(&mut recv).next().await,
                Some(Poll::Pending)
            ));

            drop(stream_tx);
            assert!(matches!(poll_immediate(&mut recv).next().await, None));
        }

        let mut setup1 = Setup::new(HelperIdentity::ONE);
        let mut setup2 = Setup::new(HelperIdentity::TWO);

        setup1.connect(&mut setup2);

        let transport1 = setup1.start(TransportCallbacks::default());
        let transport2 = setup2.start(TransportCallbacks::default());
        let transports = HashMap::from([
            (HelperIdentity::ONE, Arc::downgrade(&transport1)),
            (HelperIdentity::TWO, Arc::downgrade(&transport2)),
        ]);

        send_and_verify(HelperIdentity::ONE, HelperIdentity::TWO, &transports).await;
        send_and_verify(HelperIdentity::TWO, HelperIdentity::ONE, &transports).await;
    }

    #[tokio::test]
    async fn panic_if_stream_received_twice() {
        let (tx, owned_transport) =
            Setup::new(HelperIdentity::ONE).into_active_conn(TransportCallbacks::default());
        let step = Step::from(STEP);
        let (stream_tx, stream_rx) = channel(1);
        let stream = InMemoryStream::from(stream_rx);
        let transport = Arc::downgrade(&owned_transport);

        let mut recv_stream = transport.receive(HelperIdentity::TWO, (QueryId, step.clone()));
        send_and_ack(&tx,
            Addr::records(HelperIdentity::TWO, QueryId, step.clone()),
            stream,
        )
        .await;

        stream_tx.send(vec![4, 5, 6]).await.unwrap();
        assert_eq!(vec![4, 5, 6], recv_stream.next().await.unwrap());

        // the same stream cannot be received again
        let mut err_recv = transport.receive(HelperIdentity::TWO, (QueryId, step.clone()));
        let err = AssertUnwindSafe(err_recv.next()).catch_unwind().await;
        assert_eq!(
            Some(true),
            err.unwrap_err()
                .downcast_ref::<String>()
                .map(|s| { s.contains("stream has been consumed already") })
        );

        // even after the input stream is closed
        drop(stream_tx);
        let mut err_recv = transport.receive(HelperIdentity::TWO, (QueryId, step.clone()));
        let err = AssertUnwindSafe(err_recv.next()).catch_unwind().await;
        assert_eq!(
            Some(true),
            err.unwrap_err()
                .downcast_ref::<String>()
                .map(|s| { s.contains("stream has been consumed already") })
        );
    }

    #[tokio::test]
    async fn can_consume_ordering_sender() {
        let tx = Arc::new(OrderingSender::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(2).unwrap()
        ));
        let rx = tx.clone().as_rc_stream();
        // let (tx, rx) = ordering_mpsc::<Fp31, _>("test", NonZeroUsize::new(2).unwrap());
        let network = InMemoryNetwork::default();
        let transport1 = network.transport(HelperIdentity::ONE);
        let transport2 = network.transport(HelperIdentity::TWO);

        let step = Step::from(STEP);
        transport1
            .send(
                HelperIdentity::TWO,
                (RouteId::Records, QueryId, step.clone()),
                rx,
            )
            .await
            .unwrap();
        let mut recv = transport2.receive(HelperIdentity::ONE, (QueryId, step));

        tx.send(0, Fp31::try_from(0_u128).unwrap()).await;
        // can't receive the value at index 0 because of buffering inside the sender
        assert_eq!(Some(Poll::Pending), poll_immediate(&mut recv).next().await);

        // make the sender ready
        tx.send(1, Fp31::try_from(1_u128).unwrap()).await;
        tx.close(2).await;
        // drop(tx);

        // must be received by now
        assert_eq!(vec![vec![0, 1]], recv.collect::<Vec<_>>().await);
    }
}
