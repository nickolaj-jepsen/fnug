// //std::sync::mpsc
//
// use pyo3::sync::GILOnceCell;
// use pyo3::PyObject;
// use std::sync::{mpsc, Mutex};
//
// enum UpdateType {
//     CommandListUpdate,
// }
//
// static UI_CONTROLLER: GILOnceCell<UIController> = GILOnceCell::new();
//
// /// A global singleton for sending messages via python callbacks to the UI
// struct UIController {
//     sender: mpsc::Sender<UpdateType>,
//     receiver: Mutex<mpsc::Receiver<UpdateType>>,
//     callbacks: Vec<PyObject>,
// }
//
// impl UIController {
//     fn new() -> Self {
//         let (sender, receiver) = mpsc::channel();
//         UIController {
//             sender,
//             receiver: Mutex::new(receiver),
//             callbacks: Vec::new(),
//         }
//     }
//
//     fn send_update(&self, update: UpdateType) {
//         self.sender.send(update).unwrap();
//     }
//
//     fn add_callback(&mut self, callback: PyObject) {
//         self.callbacks.push(callback);
//     }
// }
