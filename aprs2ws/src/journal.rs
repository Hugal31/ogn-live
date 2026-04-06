use anyhow::Result;
use aprs::Report;
use systemd::journal::{JournalRef, JournalWaitResult, OpenFilesOptions, OpenOptions};
use tokio::{
    io::{Interest, unix::AsyncFd},
    sync::broadcast,
};

use crate::formatter::ReportFormatter;

pub async fn read_journal(
    path: String,
    sender: broadcast::Sender<String>,
    formatter: ReportFormatter,
    unit: Option<String>,
) -> Result<()> {
    let mut journal = OpenFilesOptions::default().open_files(vec![path])?;

    if let Some(unit) = unit {
        match_unit(&mut journal, &unit)?;
    }

    while journal.next()? > 0 {
        process_journal_entry(&mut journal, &sender, &formatter).ok();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    Ok(())
}

pub async fn watch_journal(
    sender: broadcast::Sender<String>,
    formatter: ReportFormatter,
    unit: Option<String>,
) -> Result<()> {
    let mut journal = OpenOptions::default()
        .system(true)
        .local_only(true)
        .open()?;

    if let Some(unit) = unit {
        match_unit(&mut journal, &unit)?;
    }

    journal.seek_tail()?;

    journal.previous()?;
    let async_fd = AsyncFd::with_interest(journal.fd()?, Interest::READABLE)?;

    log::debug!("Loop on journal");
    loop {
        let mut guard = async_fd.readable().await?;

        let event = journal.process()?;
        log::debug!("event: {event:?}");
        match event {
            JournalWaitResult::Append => (),
            JournalWaitResult::Nop => (),
            JournalWaitResult::Invalidate => (),
        }

        while journal.next()? > 0 {
            process_journal_entry(&mut journal, &sender, &formatter).ok();
        }

        guard.clear_ready();
    }
}

fn match_unit(journal: &mut JournalRef, unit: &str) -> systemd::Result<()> {
    let unit = if unit.contains(".") {
        unit
    } else {
        &format!("{unit}.service")
    };
    journal.match_add("_SYSTEMD_UNIT", unit)?;
    Ok(())
}

fn process_journal_entry(
    journal: &mut systemd::journal::JournalRef,
    sender: &broadcast::Sender<String>,
    formatter: &ReportFormatter,
) -> Result<()> {
    if let Some(field) = journal.get_data("MESSAGE")?
        && let Some(Ok(message)) = field.value().map(std::str::from_utf8)
    {
        const APRS_MARKER: &str = "APRS <- ";
        log::debug!("msg: {message}");
        if let Some(aprs_msg) = message.strip_prefix(APRS_MARKER)
            && let Ok(report) = aprs_msg.parse::<Report>()
        {
            log::debug!("Got report {report:?}");

            if let Report::PositionReport(report) = report {
                sender.send(formatter.format(&report))?;
            }
        }
    }

    Ok(())
}
