# Positioning

Tagline: Do more with your agents

Expanded Tagline: An open source, multi-machine orchestrator built on tmux. Dispatch tasks to a team of agents locally or over SSH.

<br />

## Background

My core motivation with the project was that I was getting tired of managing separate workstreams in terminal tabs each running their own agents. I sometimes found myself sending the wrong thing to an agent just because I picked the wrong Terminal tab to respond in. I also found myself forgetting about tabs where they would be paused for a long time waiting for the answer to a question. Shelbi removes the pain of manually orchestrating the agents and allows me to focus more on giving them the right direction on the plan and task level.

<br />

## How I work

My main method is to first create a markdown plan for a feature. Then when I'm satisfied with the plan I ask the Orchestrator to break it into tasks and distribute them to the worker agents.\
\
If you're using sub-agents with Claude (most people are even if they don't realize it now) then you're already getting some of the benefits of parallelization.\
\
Where Shelbi shines  is in giving the agents discrete chunks of work. Task-level turns out to be an extremely helpful resolution for worker agents. By breaking large plans down into tasks, and creating one off tasks for smaller work, Shelbi can balance large and small things well leading to greater personal productivity.

<br />

As far as review, yes using Shelbi you might have more code to review, but if you work on the systems a bit you can probably worry less about the code and trust the agents to do their jobs.\
\
What I mean is Shelbi provides some of the primitives you need to support workflows that produce extremely high quality code. For example, you can create specialized agents to do specific tasks like Adversarial Code review, QA, and Security. Each agent can govern a column on the board so that every task benefits from the same scrutiny. The net effect is that work can be delivered at a higher quality bar, not lower. The key is building that bar into the system.
