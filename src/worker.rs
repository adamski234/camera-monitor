use std::thread::{self, JoinHandle};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use slint::{Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, StandardListViewItem, VecModel, Weak};
use tokio::sync::{mpsc::{self, Receiver}, oneshot::channel};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use futures_util::{SinkExt, StreamExt};

pub enum WorkerMessage {
	AttemptLogin(LoginData),
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
	let client = reqwest::ClientBuilder::new().build().unwrap();
	loop {
		let message = message_receiver.recv().await.unwrap();
		match message {
			WorkerMessage::AttemptLogin(login_data) => {
				let (sender, receiver) = channel();
				window_ref.upgrade_in_event_loop(move |window| {
					sender.send(window.get_server_address().to_string()).unwrap();
				}).unwrap();
				let server_address = receiver.await.unwrap();
				let response = client.post(format!("http://{}/login", server_address)).json(&login_data).send().await.unwrap();
				if response.status() != StatusCode::OK {
					window_ref.upgrade_in_event_loop(|window| {
						window.set_error_message(SharedString::from("Error when registering"));
					}).unwrap();
				} else {
					// token is passed in the authorization header
					let token = response.headers().get("Authorization").unwrap().to_str().unwrap();
					println!("Received login token: {}", token);
					let username_response = client.get(format!("http://{}/self_user", server_address)).bearer_auth(token).send().await.unwrap();
					let username = username_response.json::<UserInfoResponseData>().await.unwrap().username;
					println!("Received username: {}", username);
					window_ref.upgrade_in_event_loop(|window| {
						window.set_state(crate::WindowState::Cameras);
					}).unwrap();
					tokio::spawn(camera_list_task(window_ref.clone(), cancel_token.clone(), username, server_address.clone()));
				}
			}
			WorkerMessage::AttemptRegister(reg_data) => {
				let (sender, receiver) = channel();
				window_ref.upgrade_in_event_loop(move |window| {
					sender.send(window.get_server_address().to_string()).unwrap();
				}).unwrap();
				let server_address = receiver.await.unwrap();
				let cloned_client = client.clone();
				let window_ref = window_ref.clone();
				tokio::spawn(async move {
					println!("http://{}/register/user", server_address);
					let response = cloned_client.post(format!("http://{}/register/user", server_address)).json(&reg_data).send().await.unwrap();
					if response.status() != StatusCode::OK {
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

async fn camera_list_task(window_ref: Weak<crate::MainWindow>, cancel_token: CancellationToken, username: String, server_address: String) {
	let (socket_connection, _) = tokio_tungstenite::connect_async(format!("ws://{}/web-socket?username={}", server_address, username)).await.unwrap();
	let (mut writer, mut reader) = socket_connection.split();
	let (cam_changer_send, mut cam_changer_read) = mpsc::channel(5);
	let window_clone = window_ref.clone();
	window_ref.upgrade_in_event_loop(move |window| {
		window.on_change_camera(move |index| {
			let strong_window = window_clone.upgrade().unwrap();
			let label = strong_window.get_cameras().row_data(index as usize).unwrap();
			let label_vec = label.text.trim_start_matches('[').trim_end_matches(']').split_ascii_whitespace().map(|v| v.trim_end_matches(',').parse::<u8>().unwrap()).collect::<Vec<_>>();
			cam_changer_send.blocking_send(label_vec).unwrap();
		});
	}).unwrap();
	tokio::spawn(async move  {
		loop {
			let new_camera_id = cam_changer_read.recv().await.unwrap();
			let data_to_send = DeviceSelectPacket { message_type: String::from("select_device"), device_id: DeviceId { content_type: String::from("Buffer"), data: new_camera_id } };
			writer.send(Message::Text(serde_json::to_string(&data_to_send).unwrap())).await.unwrap();
		}
	});
	loop {
		tokio::select! {
			_ = cancel_token.cancelled() => {
				return;
			}
			data = reader.next() => {
				let data = data.unwrap().unwrap();
				handle_websocket_message(window_ref.clone(), data).await;
			}
		}
	}
}

async fn handle_websocket_message(window_ref: Weak<crate::MainWindow>, message: Message) {
	if message.is_binary() {
		// Received a frame
		let frame_data = message.into_data();
		let mut image = zune_jpeg::JpegDecoder::new(frame_data);
		let image_bytes = image.decode().unwrap();
		let (image_width, image_height) = image.dimensions().unwrap();
		let mut new_buffer = SharedPixelBuffer::<Rgb8Pixel>::new(image_width as u32, image_height as u32);
		new_buffer.make_mut_bytes().copy_from_slice(&image_bytes);
		window_ref.upgrade_in_event_loop(move |window| {
			window.set_frame(Image::from_rgb8(new_buffer));
		}).unwrap();
	} else if message.is_text() {
		let message = message.into_text().unwrap();
		// currently there is only one text message type and that is the device list
		let list;
		match serde_json::from_str::<DeviceList>(&message) {
			Ok(result) => list = result,
			Err(_) => {
				println!("Received unknown packet: {}", message);
				return;
			}
		};
		let list_items = list.devices.into_iter().map(|device| format!("{:?}", device.id.data)).map(|id| StandardListViewItem::from(id.as_str())).collect::<Vec<_>>();
		window_ref.upgrade_in_event_loop(move |window| {
			window.set_cameras(ModelRc::new(VecModel::from(list_items)));
		}).unwrap();
	}
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegistrationData {
	pub username: String,
	pub email: String,
	pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginData {
	pub email: String,
	pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct UserInfoResponseData {
	username: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct DeviceId {
	#[serde(rename = "type")]
	content_type: String,
	data: Vec<u8>,
}

#[derive(Debug, Deserialize, Serialize)]
struct DeviceData {
	id: DeviceId,
	#[serde(rename = "createdAt")]
	created_at: String
}

#[derive(Debug, Deserialize, Serialize)]
struct DeviceList {
	#[serde(rename = "type")]
	message_type: String,
	devices: Vec<DeviceData>
}

#[derive(Debug, Deserialize, Serialize)]
struct DeviceSelectPacket {
	#[serde(rename = "type")]
	message_type: String,
	device_id: DeviceId
}