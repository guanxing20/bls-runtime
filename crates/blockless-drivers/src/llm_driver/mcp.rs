use crate::llm_driver::LlmOptions;
use anyhow::Result;
use rmcp::{
    ServiceExt,
    model::Tool,
    model::{CallToolRequestParam, ClientCapabilities, ClientInfo, Implementation},
    transport::SseTransport,
};
use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, error, info, warn};
use url::Url;

const DEFAULT_SYSTEM_MESSAGE: &str = "You are a helpful AI assistant.";

/// Constructs the system prompt with potential tools map
pub async fn construct_system_prompt_with_tools(
    options: &LlmOptions,
) -> (String, Option<ToolsMap>) {
    // Generate the system prompt with more detailed date format
    let today_date = chrono::Local::now().format("%B %d, %Y").to_string();

    // Start with date prompt
    let mut system_prompt = format!("Today Date: {}\n", today_date);

    // Process MCP tool URLs if provided, populate the tools map
    let mut tools_map = None;
    if let Some(urls) = &options.tools_sse_urls {
        if !urls.is_empty() {
            let map = get_tools_map(urls).await;
            info!("Loaded {} MCP tools from {} URLs", map.len(), urls.len());
            tools_map = Some(map);
        }
    }

    // Structure prompt based on tools availability
    if let Some(map) = &tools_map {
        if !map.is_empty() {
            let tools_json = map
                .values()
                .map(|tool_info| &tool_info.tool)
                .collect::<Vec<_>>();

            // Assistant instructions for tool-enabled prompt
            system_prompt.push_str(r#"
# Assistant Instructions
You are a helpful AI assistant that can leverage tools to provide accurate and up-to-date information. You excel at understanding when and how to use the right tools to solve user requests efficiently.
"#);

            // Add custom system message if provided
            if let Some(system_message) = &options.system_message {
                system_prompt.push_str(&format!("{}\n", system_message));
            }

            // Tool instructions - more detailed guidance
            system_prompt.push_str(&format!(r#"
# Tool Instructions
You have access to external tools that can provide real-time information, perform calculations, and execute specific actions. Always consider using these tools when:
- The user asks for real-time or up-to-date information
- The user's request requires external data that may not be in your training
- A specialized capability would provide a more accurate or detailed response
- You need to verify facts or check current information

Available functions:
{}

Function calling protocol:
```
<function>{{ "name": "example_function_name", "arguments": {{ "example_name": "example_value" }} }}</function>
```

Critical requirements for function calls:
- Function calls MUST be enclosed in BOTH <function> and </function> tags
- All required parameters MUST be specified in the arguments object
- When calling a function, respond ONLY with the function call JSON object
- Keep the entire function call on a single line
- Call only one function at a time
- Include exact parameter names as specified in the function definition
- When using search results in non-function responses, always cite your sources

For complex requests, use a step-by-step approach:
1. Analyze what tool is most appropriate for the request
2. Call the function with precise parameters
3. Use the returned data to formulate your complete response

When explicitly asked to use MCP (Model Context Protocol), you MUST use the functions provided to you.
"#, serde_json::to_string_pretty(&tools_json).unwrap_or_default()));
            debug!("System prompt with tools: {}", system_prompt);
        } else {
            // No tools available case
            system_prompt.push_str(&format!(
                "# Assistant Instructions\n{}",
                options
                    .system_message
                    .clone()
                    .unwrap_or(DEFAULT_SYSTEM_MESSAGE.to_string())
            ));
        }
    } else {
        // No tools map case
        system_prompt.push_str(&format!(
            "# Assistant Instructions\n{}",
            options
                .system_message
                .clone()
                .unwrap_or(DEFAULT_SYSTEM_MESSAGE.to_string())
        ));
    }

    (system_prompt, tools_map)
}

/// Result of processing a potential function call
#[derive(Debug)]
pub enum ProcessFunctionResult {
    NoFunctionCall,
    FunctionExecuted(String),
    Error(String),
}

/// Process a response from an LLM to detect and execute function calls
///
/// This function is stateless and handles the entire function call lifecycle:
/// 1. Detects if a function call is present in the content by
///    stripping everything before until opening bracket and everything after until closing bracket
/// 2. Parses the function name and arguments
/// 3. Executes the function call if valid
///
/// Returns a result indicating whether a function was called and the result
pub async fn process_function_call(content: &str, tools_map: &ToolsMap) -> ProcessFunctionResult {
    debug!("Function call content before processing: {}", content);

    // Extract JSON content between first '{' and last '}'
    // Ensure both braces are found and end comes after start
    let start_idx = content.find('{').unwrap_or(0);
    let end_idx = content.rfind('}').unwrap_or(content.len());
    // Safety check to ensure both indexes are valid and end > start
    let fn_content = if start_idx < end_idx && end_idx < content.len() {
        &content[start_idx..=end_idx]
    } else if start_idx < content.len() {
        // Handle case where '}' is not found but '{' is
        &content[start_idx..]
    } else {
        content
    };

    debug!("Function call content: {}", fn_content);

    // Extract function JSON
    let fn_call: Value = match serde_json::from_str(fn_content) {
        Ok(call) => call,
        Err(err) => {
            debug!("failed to parse function call string to JSON: {}", err);
            return ProcessFunctionResult::NoFunctionCall;
        }
    };

    // Extract function name and arguments
    let (name, args) = match (
        fn_call.get("name").and_then(|n| n.as_str()),
        fn_call.get("arguments"),
    ) {
        (Some(name), Some(args)) => (name, args.clone()),
        _ => return ProcessFunctionResult::NoFunctionCall,
    };

    info!("Detected function call: {}", name);

    // Execute the function call
    match call_tool(name, args, tools_map).await {
        Ok(result) => {
            debug!("Function '{}' result:\n{}", name, result);
            ProcessFunctionResult::FunctionExecuted(result)
        }
        Err(e) => {
            let error_msg = format!("Error calling function '{}': {}", name, e);
            ProcessFunctionResult::Error(error_msg)
        }
    }
}

/// Maps to a similar structure as the TypeScript getToolsMap function
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub url: Url,
    pub tool: Tool,
    pub is_accessible: bool,
}

pub type ToolsMap = HashMap<String, ToolInfo>;

/// Validates MCP SSE URLs and returns a map of tools
/// The key is the tool name, and the value is an object which contains the URL of the MCP SSE and the tool definition
/// This is infallible, so it will return a ToolsMap even if there are no tools; invalid URLs are ignored
/// - TODO: utilize client.cancel() to cancel the tool list if timeout is reached
async fn get_tools_map(tools_sse_urls: &[String]) -> ToolsMap {
    let tools_sse_urls = tools_sse_urls
        .iter()
        .filter_map(|s| match Url::parse(s) {
            Ok(url) => Some(url),
            Err(e) => {
                error!("Invalid URL {}: {:?}", s, e);
                None
            }
        })
        .collect::<Vec<_>>();

    let mut tools_map: ToolsMap = HashMap::new();

    for url in &tools_sse_urls {
        debug!("Testing MCP SSE URL: {}", url);

        let client_info = ClientInfo {
            protocol_version: Default::default(),
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "blockless-mcp-client".to_string(),
                version: "1.0.0".to_string(),
            },
        };

        let transport = match SseTransport::start(url.clone()).await {
            Ok(transport) => transport,
            Err(e) => {
                warn!("Failed to start transport for MCP SSE URL {}: {:?}", url, e);
                continue;
            }
        };

        let client = match client_info.serve(transport).await.inspect_err(|e| {
            tracing::error!("client error: {:?}", e);
        }) {
            Ok(client) => client,
            Err(e) => {
                warn!("Failed to start client for MCP SSE URL {}: {:?}", url, e);
                continue;
            }
        };

        // Initialize
        let server_info = client.peer_info();
        tracing::info!("Connected to server: {server_info:#?}");

        // List tools
        let tools_response = match client.list_tools(Default::default()).await {
            Ok(tools_response) => tools_response,
            Err(e) => {
                warn!("Failed to list tools for MCP SSE URL {}: {:?}", url, e);
                continue;
            }
        };

        debug!("Available tools: {tools_response:#?}");

        // Add each tool to the map
        for tool in tools_response.tools {
            debug!("Adding tool: {}", tool.name);
            tools_map.insert(
                tool.name.to_string(),
                ToolInfo {
                    url: url.clone(),
                    tool,
                    is_accessible: true,
                },
            );
        }

        match client.cancel().await {
            Ok(_) => (),
            Err(e) => {
                warn!("Failed to cancel client for MCP SSE URL {}: {:?}", url, e);
                continue;
            }
        }
    }

    info!(
        "Validated {} tools from {} MCP SSE URLs",
        tools_map.len(),
        tools_sse_urls.len()
    );
    tools_map
}

/// Calls a tool through the MCP protocol
/// - TODO: utilize client.cancel() to cancel the tool call if timeout is reached
async fn call_tool(
    tool_name: &str,
    arguments: serde_json::Value,
    tools_map: &ToolsMap,
) -> Result<String> {
    let Some(tool_info) = tools_map.get(tool_name) else {
        anyhow::bail!("Tool {} not found", tool_name)
    };
    let url_str = tool_info.url.as_str();

    info!(
        "Calling tool: `{}` fn:`{}` args:`{}`",
        url_str, tool_name, arguments
    );

    let transport = SseTransport::start(url_str).await?;
    let client_info = ClientInfo {
        protocol_version: Default::default(),
        capabilities: ClientCapabilities::default(),
        client_info: Implementation {
            name: "blockless-mcp-client".to_string(),
            version: "1.0.0".to_string(),
        },
    };
    let client = client_info.serve(transport).await.inspect_err(|e| {
        error!("Client error when calling tool {}: {:?}", tool_name, e);
    })?;
    let args = arguments.as_object().cloned();

    let tool_result = client
        .call_tool(CallToolRequestParam {
            name: tool_name.to_string().into(),
            arguments: args,
        })
        .await?;
    client.cancel().await?;

    if tool_result.is_error.unwrap_or(false) {
        anyhow::bail!("Tool {} returned an error", tool_name);
    }

    let result = tool_result
        .content
        .into_iter()
        .filter_map(|c| c.raw.as_text().map(|t| t.text.to_owned()))
        .collect::<Vec<_>>()
        .join(" ");

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;
    use tokio;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    // Only initialize the tracing subscriber once
    static INIT: Once = Once::new();
    fn init_tracing() {
        INIT.call_once(|| {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "debug".into()),
                )
                .with(tracing_subscriber::fmt::layer())
                .init();
        });
    }

    #[tokio::test]
    #[ignore = "requires local MCP servers"]
    async fn test_get_tools_map() {
        init_tracing();

        let tools_sse_urls = vec![
            "http://localhost:3001/sse".to_string(),
            "http://localhost:3002/sse".to_string(),
        ];
        let tools_map = get_tools_map(&tools_sse_urls).await;

        // Log the tools found
        info!("Found {} tools", tools_map.len());
        for (name, info) in &tools_map {
            info!("Tool: {} at {}", name, info.url);
        }

        // Validate that we have at least one tool
        assert!(!tools_map.is_empty(), "No tools found");
    }

    #[tokio::test]
    #[ignore = "requires local MCP servers"]
    async fn test_call_tool() {
        init_tracing();

        let tools_sse_urls = vec!["http://localhost:3001/sse".to_string()];
        let tools_map = get_tools_map(&tools_sse_urls).await;

        assert!(
            !tools_map.is_empty(),
            "Run local MCP server at http://localhost:3001/sse to test"
        );

        // Construct call for first tool
        // Assume `add` function with two arguments `a` and `b`
        // Return the result of `a + b` as a single string value
        let (tool_name, _) = tools_map.iter().next().unwrap();
        let arguments = serde_json::json!({ "a": 1, "b": 2 });

        // Call the tool
        let result = call_tool(tool_name, arguments, &tools_map).await.unwrap();

        info!("Tool call result: {:?}", result);

        // Validate that we got a value from the tool
        assert!(!result.is_empty(), "Tool call failed");

        // Validate that the value is correct
        assert_eq!(result, "3");
    }

    #[tokio::test]
    async fn test_process_function_call_returns_no_function_call() {
        // Initialize logging
        init_tracing();

        let content = r#"To add the numbers 1215 and 2213, I can use the following calculation:
    1215 + 2213 = 3438
    Sources: Basic arithmetic operations.<|eot_id|>
"#;
        let result = process_function_call(content, &HashMap::new()).await;
        assert!(matches!(result, ProcessFunctionResult::NoFunctionCall));

        let content = r#"{To add the numbers 1215 and 2213, I can use the following calculation:
    1215 + 2213 = 3438
    Sources: Basic arithmetic operations.<|eot_id|>
"#;
        let result = process_function_call(content, &HashMap::new()).await;
        assert!(matches!(result, ProcessFunctionResult::NoFunctionCall));

        let content = r#"{To add the numbers 1215 and 2213, I can use the following calculation:
        1215 + 2213 = 3438
        Sources: Basic arithmetic operations.<|eot_id|>
}"#;
        let result = process_function_call(content, &HashMap::new()).await;
        assert!(matches!(result, ProcessFunctionResult::NoFunctionCall));
    }

    #[tokio::test]
    async fn test_process_function_call_returns_error() {
        // Initialize logging
        init_tracing();

        // valid json; but mcp-server not running
        let content = r#"{ "name": "divide", "arguments": { "a": 1, "b": 2 } }"#;
        let result = process_function_call(content, &HashMap::new()).await;
        assert!(matches!(result, ProcessFunctionResult::Error(_)));
    }

    #[tokio::test]
    #[ignore = "requires local MCP server with add function"]
    async fn test_process_function_call() {
        // Initialize logging
        init_tracing();

        let tools_sse_urls = vec!["http://localhost:3001/sse".to_string()];
        let tools_map = get_tools_map(&tools_sse_urls).await;

        // Skip test if no tools found
        assert!(
            !tools_map.is_empty(),
            "Run local MCP server at http://localhost:3001/sse to test"
        );

        // Construct call for first tool
        let content = r#"{ "name": "add", "arguments": { "a": 9, "b": 10 } }"#;
        let result = process_function_call(content, &tools_map).await;
        assert!(matches!(result, ProcessFunctionResult::FunctionExecuted(_)));
        let result = match result {
            ProcessFunctionResult::FunctionExecuted(result) => result,
            _ => unreachable!(),
        };
        assert_eq!(result, "19");
    }

    #[tokio::test]
    #[ignore = "requires local MCP server with add function"]
    async fn test_process_function_call_with_enclosed_function() {
        // Initialize logging
        init_tracing();

        let tools_sse_urls = vec!["http://localhost:3001/sse".to_string()];
        let tools_map = get_tools_map(&tools_sse_urls).await;

        // Skip test if no tools found
        assert!(
            !tools_map.is_empty(),
            "Run local MCP server at http://localhost:3001/sse to test"
        );

        // Construct call for first tool
        let content = r#"<function>{ "name": "add", "arguments": { "a": 1, "b": 2 } }</function>"#;
        let result = process_function_call(content, &tools_map).await;
        assert!(matches!(result, ProcessFunctionResult::FunctionExecuted(_)));
        let result = match result {
            ProcessFunctionResult::FunctionExecuted(result) => result,
            _ => unreachable!(),
        };
        assert_eq!(result, "3");
    }
}
