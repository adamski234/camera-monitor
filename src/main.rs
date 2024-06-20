use tokio::sync::mpsc::channel;
use tokio_util::sync::CancellationToken;
use worker::RegistrationData;

mod worker;

slint::include_modules!();

fn main() {
    let window = MainWindow::new().unwrap();
	let (sender, receiver) = channel(100);
	let cancel_token = CancellationToken::new();

	let sender_clone = sender.clone();
	window.on_register(move |username, email, password| {
		sender_clone.blocking_send(worker::WorkerMessage::AttemptRegister(RegistrationData { username: username.to_string(), email: email.to_string(), password: password.to_string() })).unwrap();
	});

	let worker_thread = worker::spawn_background_worker(window.as_weak(), receiver, cancel_token.clone());

	window.run().unwrap();
	cancel_token.cancel();
	worker_thread.join().unwrap();
}
