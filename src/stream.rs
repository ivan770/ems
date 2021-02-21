use std::{convert::TryInto, time::Duration};

use audiosocket::{AudioSocketError, Message, RawMessage};
use tokio::{
    io::{AsyncRead, AsyncReadExt, BufReader},
    time::timeout,
};
use tracing::{debug, instrument, warn};

use crate::server::ServerError;

/// Inner [`BufReader`] capacity for reading data from AudioSocket client.
const AUDIO_BUF_CAPACITY: usize = 16 * 1024;

/// Stream wrapper, that produces `AudioSocket` messages from bytes.
pub struct MessageStream<'s, S> {
    buf: Vec<u8>,
    stream: BufReader<&'s mut S>,
}

impl<'s, S> MessageStream<'s, S>
where
    S: AsyncRead + Unpin,
{
    /// Create new [`MessageStream`] from provided stream.
    pub fn new(stream: &'s mut S) -> Self {
        MessageStream {
            buf: Vec::new(),
            stream: BufReader::with_capacity(AUDIO_BUF_CAPACITY, stream),
        }
    }

    /// Receive new AudioSocket message from stream.
    ///
    /// Accepts awaiting timeout as a first argument.
    #[instrument(skip(self))]
    pub async fn recv(&mut self, max_time: Duration) -> Result<Message<'_>, ServerError> {
        self.buf.clear();

        // For now, we'll wait just for the first byte.
        // If you have any troubles with this approach, please open a new issue.
        let message_type = timeout(max_time, self.stream.read_u8()).await??;
        let length = self.stream.read_u16().await?;
        let read = (&mut self.stream)
            .take(u64::from(length))
            .read_to_end(&mut self.buf)
            .await?;

        debug!(%message_type, %length, %read, "Obtained new message");

        if read != usize::from(length) {
            warn!(%length, %read, "Expected payload length and actual payload length are different");
        }

        Ok(RawMessage::from_parts(
            message_type
                .try_into()
                .map_err(AudioSocketError::IncorrectMessageType)?,
            (read != 0).then(move || &*self.buf),
        )
        .try_into()?)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        convert::TryInto,
        io::{Cursor, Result},
        pin::Pin,
        task::{Context, Poll},
        time::Duration,
    };

    use audiosocket::Message;
    use tokio::io::{AsyncRead, ReadBuf};

    use super::MessageStream;
    use crate::{handler::CHUNK_SIZE, server::ServerError};

    struct Pending;

    impl AsyncRead for Pending {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<Result<()>> {
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn audio_silence() {
        let silence = Message::Audio(Some(&[0; CHUNK_SIZE]));

        let mut cursor = Cursor::<Vec<u8>>::new(silence.try_into().unwrap());

        let mut stream = MessageStream::new(&mut cursor);

        let recv = stream.recv(Duration::from_secs(2)).await.unwrap();

        assert_eq!(recv, silence);
    }

    #[tokio::test]
    async fn timeout() {
        let mut pending = Pending;

        let mut stream = MessageStream::new(&mut pending);

        let recv = stream.recv(Duration::from_secs(2)).await.unwrap_err();

        assert!(matches!(recv, ServerError::TimeoutError(_)));
    }
}
