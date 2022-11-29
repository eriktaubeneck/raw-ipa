use std::iter::{repeat, zip};

use async_trait::async_trait;
use futures::future::try_join_all;

use crate::{error::Error, helpers::Role, protocol::RecordId};
use crate::{ff::Field, protocol::context::Context, secret_sharing::SecretSharing};

#[allow(dead_code)]
async fn reshare_objects<F: Field, S, C, T>(
    input: &[T],
    ctx: C,
    to_helper: Role,
) -> Result<Vec<T>, Error>
where
    C: Context<F, Share = S> + std::marker::Send,
    S: SecretSharing<F>,
    T: Resharable<F, Share = S>,
{
    let reshares = zip(repeat(ctx), input)
        .enumerate()
        .map(|(index, (ctx, input))| async move {
            input
                .resharable(ctx, RecordId::from(index), to_helper)
                .await
        });
    try_join_all(reshares).await
}

#[async_trait]
pub trait Resharable<F: Field>: Sized {
    type Share: SecretSharing<F>;

    async fn resharable<C>(
        &self,
        ctx: C,
        record_id: RecordId,
        to_helper: Role,
    ) -> Result<Self, Error>
    where
        C: Context<F, Share = <Self as Resharable<F>>::Share> + std::marker::Send;
}
