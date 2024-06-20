use std::thread::{self, JoinHandle};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use slint::{SharedString, Weak};
use tokio::sync::{mpsc::Receiver, oneshot::channel};
use tokio_util::sync::CancellationToken;

pub enum WorkerMessage {
	AttemptLogin { email: String, password: String },
	AttemptRegister(RegistrationData),
}


pub fn spawn_background_worker(window_ref: Weak<crate::MainWindow>, message_receiver: Receiver<WorkerMessage>, cancel_token: CancellationToken) -> JoinHandle<()> {
	let handle = thread::spawn(move || {
		tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(async {
			let task = tokio::spawn(worker_task(window_ref, message_receiver, cancel_token.clone()));
			tokio::select! {
				_ = cancel_token.cancelled() => {
					println!("Cancellation token received");
					return;
				}
				_ = task => {
					println!("Worker task quit");
					return;
				}
			};
		});
	});
	return handle;
}

async fn worker_task(window_ref: Weak<crate::MainWindow>, mut message_receiver: Receiver<WorkerMessage>, cancel_token: CancellationToken) {
	let mut auth_token = String::new();
	let mut server_address = String::new();
	let client = reqwest::ClientBuilder::new().build().unwrap();
	loop {
		let message = message_receiver.recv().await.unwrap();
		match message {
			WorkerMessage::AttemptLogin { email, password } => {
				//
			}
			WorkerMessage::AttemptRegister(reg_data) => {
				let (sender, receiver) = channel();
				window_ref.upgrade_in_event_loop(move |window| {
					sender.send(window.get_server_address().to_string()).unwrap();
				}).unwrap(); //sync
				server_address = receiver.await.unwrap();
				let cloned_client = client.clone();
				let window_ref = window_ref.clone();
				tokio::spawn(async move {
					println!("http://{}/register/user", server_address);
					let request = cloned_client.post(format!("http://{}/register/user", server_address)).json(&reg_data).send().await.unwrap();
					if request.status() != StatusCode::OK {
						window_ref.upgrade_in_event_loop(|window| {
							window.set_error_message(SharedString::from("Error when registering"));
						}).unwrap();
					} else {
						window_ref.upgrade_in_event_loop(|window| {
							window.set_state(crate::WindowState::Login);
						}).unwrap();
					}
				}).await.unwrap();
			}
		}
	}
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegistrationData {
	pub username: String,
	pub email: String,
	pub password: String,
}