use crate::error::Result;

#[cfg(not(target_arch = "wasm32"))]
pub trait Transport: Send + Sync {
    /// Establishes a connection
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established
    fn connect(&mut self) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Reads data into the provided buffer
    ///
    /// # Errors
    ///
    /// Returns an error if the read operation fails
    fn read(&mut self, buf: &mut [u8]) -> impl std::future::Future<Output = Result<usize>> + Send;

    /// Writes data from the provided buffer
    ///
    /// # Errors
    ///
    /// Returns an error if the write operation fails
    fn write(&mut self, buf: &[u8]) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Closes the connection
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be closed cleanly
    fn close(&mut self) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Checks if the transport is connected
    fn is_connected(&self) -> bool {
        false
    }
}

#[cfg(target_arch = "wasm32")]
pub trait Transport {
    /// Establishes a connection
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established
    fn connect(&mut self) -> impl std::future::Future<Output = Result<()>>;

    /// Reads data into the provided buffer
    ///
    /// # Errors
    ///
    /// Returns an error if the read operation fails
    fn read(&mut self, buf: &mut [u8]) -> impl std::future::Future<Output = Result<usize>>;

    /// Writes data from the provided buffer
    ///
    /// # Errors
    ///
    /// Returns an error if the write operation fails
    fn write(&mut self, buf: &[u8]) -> impl std::future::Future<Output = Result<()>>;

    /// Closes the connection
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be closed cleanly
    fn close(&mut self) -> impl std::future::Future<Output = Result<()>>;

    /// Checks if the transport is connected
    fn is_connected(&self) -> bool {
        false
    }
}
