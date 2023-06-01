use embedded_hal_async::spi::SpiDevice;

use crate::socket;
use crate::spi::SpiInterface;

pub const MODE: u16 = 0x00;
pub const MAC: u16 = 0x09;
pub const SOCKET_INTR: u16 = 0x16;
pub const PHY_CFG: u16 = 0x3c;

pub const TX_BASE: u16 = 0x4000;
pub const RX_BASE: u16 = 0x6000;
pub const BUF_SIZE: u16 = 0x2000; // 8kb, for each tx and rx

/// W5500 in MACRAW mode
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct W5500<SPI> {
    bus: SpiInterface<SPI>,
}

impl<SPI: SpiDevice> W5500<SPI> {
    /// Create and initialize the W5500 driver
    pub async fn new(spi: SPI, mac_addr: [u8; 6]) -> Result<W5500<SPI>, SPI::Error> {
        let mut bus = SpiInterface(spi);
        // Reset device
        bus.write_frame(MODE, &[0x80]).await?;

        // Enable interrupt pin
        bus.write_frame(SOCKET_INTR, &[0x01]).await?;
        // Enable receive interrupt
        bus.write_frame(socket::SOCKET_INTR_MASK, &[socket::Interrupt::Receive as u8])
            .await?;

        // Set MAC address
        bus.write_frame(MAC, &mac_addr).await?;

        // Set the raw socket RX/TX buffer sizes to  16KB
        bus.write_frame(socket::TXBUF_SIZE, &[8]).await?;
        bus.write_frame(socket::RXBUF_SIZE, &[8]).await?;

        // MACRAW mode with MAC filtering.
        let mode: u8 = (1 << 2) | (1 << 6);
        bus.write_frame(socket::MODE, &[mode]).await?;
        socket::command(&mut bus, socket::Command::Open).await?;

        Ok(Self { bus })
    }

    /// Read bytes from the RX buffer. Returns the number of bytes read.
    async fn read_bytes(&mut self, read_ptr: &mut u16, buffer: &mut [u8]) -> Result<(), SPI::Error> {
        let addr = *read_ptr % BUF_SIZE;
        if addr as usize + buffer.len() <= BUF_SIZE as usize {
            self.bus.read_frame(RX_BASE + addr, buffer).await?;
        } else {
            let n = BUF_SIZE - addr;
            self.bus.read_frame(RX_BASE + addr, &mut buffer[..n as usize]).await?;
            self.bus.read_frame(RX_BASE, &mut buffer[n as usize..]).await?;
        }

        *read_ptr = (*read_ptr).wrapping_add(buffer.len() as u16);

        Ok(())
    }

    /// Read an ethernet frame from the device. Returns the number of bytes read.
    pub async fn read_frame(&mut self, frame: &mut [u8]) -> Result<usize, SPI::Error> {
        let rx_size = socket::get_rx_size(&mut self.bus).await? as usize;
        if rx_size == 0 {
            return Ok(0);
        }

        socket::reset_interrupt(&mut self.bus, socket::Interrupt::Receive).await?;

        let mut read_ptr = socket::get_rx_read_ptr(&mut self.bus).await?;

        // First two bytes gives the size of the received ethernet frame
        let expected_frame_size: usize = {
            let mut frame_bytes = [0u8; 2];
            self.read_bytes(&mut read_ptr, &mut frame_bytes).await?;
            u16::from_be_bytes(frame_bytes) as usize - 2
        };

        // Read the ethernet frame
        self.read_bytes(&mut read_ptr, &mut frame[..expected_frame_size])
            .await?;

        // Register RX as completed
        socket::set_rx_read_ptr(&mut self.bus, read_ptr).await?;
        socket::command(&mut self.bus, socket::Command::Receive).await?;

        Ok(expected_frame_size)
    }

    /// Write an ethernet frame to the device. Returns number of bytes written
    pub async fn write_frame(&mut self, frame: &[u8]) -> Result<usize, SPI::Error> {
        let free = socket::get_tx_free_size(&mut self.bus).await?;
        while free < frame.len() as u16 {}

        let write_ptr: u16 = socket::get_tx_write_ptr(&mut self.bus).await?;

        let addr = write_ptr % BUF_SIZE;
        if addr as usize + frame.len() <= BUF_SIZE as usize {
            self.bus.write_frame(TX_BASE + addr, frame).await?;
        } else {
            let n = BUF_SIZE - addr;
            self.bus.write_frame(TX_BASE + addr, &frame[..n as usize]).await?;
            self.bus.write_frame(TX_BASE, &frame[n as usize..]).await?;
        }

        socket::set_tx_write_ptr(&mut self.bus, write_ptr.wrapping_add(frame.len() as u16)).await?;
        socket::command(&mut self.bus, socket::Command::Send).await?;
        Ok(frame.len())
    }

    pub async fn is_link_up(&mut self) -> bool {
        let mut link = [0];
        self.bus.read_frame(PHY_CFG, &mut link).await.ok();
        link[0] & 1 == 1
    }
}
