/*
 * local TCP socket client inside target proccess that conects to lua-dap-server
 *
 * transmit execution state
 * stack frames
 * variable inspections
 */
use crate::types::{AgentToDapMessage, DapToAgentCommand};

pub struct IpcClient {
    addr: String,
}

impl IpcClient {
    /*
     * establish tcp stream to lua-dap-server
     */
    pub fn connect(addr: &str) -> Result<Self, std::io::Error> {}

    /*
     * serialize agent events to JSON and send
     */
    pub fn send_message(&mut self, msg: &AgentToDapMessage) -> Result<(), std::io::Error> {}

    /*
     * run agent's background OS thread to poll commands from lua-dap-server
     */
    pub fn listen_loop(&mut self, command_sender: CrossbeamSender<DapToAgentCommand>) {}

    /*
     * deserialize JSON payloads into typed command structs
     */
    pub fn process_incoming_command(
        raw_json: &str,
    ) -> Result<DapToAgentCommand, serde_json::Error> {
    }
}
