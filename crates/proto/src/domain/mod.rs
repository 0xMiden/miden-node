pub mod account;
pub mod batch;
pub mod block;
pub mod digest;
pub mod mempool;
pub mod merkle;
pub mod note;
pub mod nullifier;
pub mod transaction;

// UTILITIES
// ================================================================================================

pub fn convert<T, From, To, R>(from: T) -> R
where
    T: IntoIterator<Item = From>,
    From: Into<To>,
    R: FromIterator<To>,
{
    from.into_iter().map(Into::into).collect()
}

pub fn try_convert<T, E, From, To, R>(from: T) -> Result<R, E>
where
    T: IntoIterator<Item = From>,
    From: TryInto<To, Error = E>,
    R: FromIterator<To>,
{
    from.into_iter().map(TryInto::try_into).collect()
}
