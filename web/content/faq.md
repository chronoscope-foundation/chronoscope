# About the project

## Why was Chronoscope built?

Buildings outlast the people who built them, which makes them the best record a place keeps of its own past. Plenty of people already go looking for it: then-and-now photographs, urban exploration, trips taken just to walk through an old city. What was missing was a way to do it at scale, across millions of images, without a community spending years annotating them first. That last part is what recent AI made possible. The [About page](/about) tells the longer story.

## How does Chronoscope work?

Chronoscope is a wiki with one rule: every claim points back to a source anyone can go and check, whether that's a photograph, an archive record, a map, or a Wikipedia article. That rule is what lets AI contribute here at all. A machine's claim and a person's claim arrive in the same form, with their evidence attached, and neither is believed on the strength of who submitted it.

Every claim also has to fit alongside the others into one coherent picture, and when one doesn't fit, that's worth knowing. Wikipedia might say a church was demolished in 1896, while a photo archive dates a picture of that same church to 1902. Both can't be true: one of the dates is wrong, the demolition didn't happen the way it was recorded, or the two records are about different buildings. Chronoscope catches the collision and raises it as an open question for contributors to research, instead of quietly picking a winner.

The same reasoning fills in what nobody wrote down. Imagine an undated photograph of two buildings you don't recognize. You can still tell one thing from it: both were standing at the same moment, whatever moment that was. That's all Chronoscope knows at first, and it holds onto it. Then someone recognizes one of the buildings, a hall built in 1923 and torn down in 1931. The photograph is now pinned to those eight years, and the second building comes along with it: it was standing somewhere in that window, so it went up before 1931, and if it was ever demolished, that happened after 1923. Nobody asserted a single fact about that second building, and Chronoscope still knows part of its history. It's the reasoning you'd do yourself with the photo in your hand, run across millions of images and redone every time new evidence arrives.

It's also what makes automated research safe to build on. A vision model shown a ruined church may announce that it's a particular Dresden church lost in the 1945 bombings: confident, plausible, and impossible to check at face value. So we don't ask it for conclusions. We ask for the smaller things anyone can check against the image itself: roof style, number of floors, which buildings sit next to each other, what a sign says. When those details fit what's already known about a place and a period, the evidence is admitted and the reasoning above does the rest. When they don't, the mismatch surfaces like any other disagreement.

## How does Chronoscope handle dates and locations that aren't precise?

Many historical sources are approximate: "circa 1920s," "somewhere near the waterfront," "between 1914 and 1918." Chronoscope keeps the vagueness as given and reasons over it directly. "Sometime in the 1920s" is a real value in its own right, and two vague dates can still contradict each other, or combine into something sharper than either alone. A value narrows only when evidence genuinely narrows it.

## What do the markers on the map mean? {#existence-states}

The map always shows a particular year, and every marker answers one question: did the sources say this place stood then?

A **solid marker** means we presume it stood. Either a source documents the place that year, or it was built earlier and nothing records its demolition. Almost nothing is documented continuously, so a palace recorded once in 1343 is presumed to stand every day after until something says otherwise. Treating that as doubt would put the whole map in doubt.

An **orange ring** means the sources disagree. A record says the building came down in 1896; a dated photograph shows it standing in 1902. Both can't be right, so we mark the place as contested and leave the question open. Open the entity to see which claims collide.

A **dashed ring** means we know where the place is but not when it stood. Nothing on record puts it standing or gone in the year you're looking at, so it stays on the map as a possibility. Evidence carries forward and not back: a building sighted in 1900 is presumed to stand ever after, and is simply unknown before it. Give that same building a construction date and scrubbing past it takes the marker away instead. A plain marker goes hollow inside the ring; one carrying a photograph fades behind it.

**Nothing at all** means the sources put its demolition before that year, or its construction after. Scrub back and the demolished ones reappear.

A **badge** is a cluster: several places too close together to draw separately at this zoom. It stands for a group and carries no answer of its own. Zoom in and it splits into individual markers, each with one.

## How does the automated image analysis work?

AI models work over each image in focused steps. A segmentation model outlines the individual buildings in a photo, drawing, or map so each can be studied separately. An embedding model indexes how each one looks, so a building in one image can surface candidates in others. A vision-language model reads what the image says about them and how they sit relative to each other, such as one building standing next to another.

None of those steps decides identity on its own. It all arrives at the reasoning engine as evidence, weighed alongside dates and locations, and that's where "have we seen this building before?" actually gets settled. Keeping the decision there rather than inside an image model makes the reasoning traceable: every conclusion points back to the evidence behind it. For the specific models we use, see our [GitHub documentation](https://github.com/copumpkin/chronoscope).

## Why not just ask Claude or GPT to do the research?

We tried. In our tests, frontier models given an unfamiliar photograph did a poor job of working out what it shows and where it was taken. They'll get better, so that isn't the reason we build it this way.

The reason is that we want a corpus you can trust. Every claim carries a citation, and we're building the machinery to check those citations automatically. That's the floor, not the ceiling: most of what's interesting in this kind of research is inferred rather than read straight off a source, and an inference is only worth trusting if you can follow the chain behind it.

It helps to think of AI involvement as a spectrum. At one end, the AI does all of it at the moment you ask: hand each image to a large model and see what it says. That takes almost no work to set up, costs a lot per image, and leaves you little to inspect when the answers don't agree with each other. At the other end sits pure symbolic reasoning, where people make the assertions and fixed rules carry them through the system. Predictable and debuggable, but expensive to build and it never stops needing people.

We landed in between, in what the industry now calls a neurosymbolic system: fixed rules do the reasoning, and AI models handle the parts nobody knows how to do deterministically, like understanding an image or reading a web page. That mix buys us a few things:

- **Better research.** We can build reasoning we trust into the process, rather than hoping a model arrives at it correctly on each image.
- **Much lower cost.** Because most of the thinking lives in our engine rather than in the model, we don't need the largest model available to look at a picture. A small one we can run ourselves does the job. Across millions of images, for a non-profit, that gap decides whether the project is possible at all.
- **Debuggability.** Every conclusion has a chain you can walk back to the evidence it rests on.
- **Knock-on effects we can afford.** When a new fact turns up about one building, we know what else it bears on and redo just that part. With a large model as the only tool, the equivalent is asking it to look at everything nearby all over again, which is precisely the cost we can't carry.

## Can the AI make mistakes?

Yes, and Chronoscope is built around that assumption. Every AI-generated claim has to cite the source it came from, and every citation is checked twice: once that the cited content really is there at the source, and again by a separate AI model judging whether that content supports the claim. Human edits go through the same checks. That makes it hard for errors, misinformation, or deliberate disinformation to spread unchecked. Hard isn't impossible, which is why Chronoscope works like a wiki: if you see something wrong, flag it or fix it. We want that to be easy, and even fun.

## How is Chronoscope different from OpenStreetMap, OpenHistoricalMap, Wikidata, Pleiades, and similar projects? {#vs-other-projects}

Most of those projects are organized around a map, a gazetteer, or a structured fact base, where the usual goal is one agreed answer per place. Chronoscope is organized around **buildings and places as entities**, and everything anyone has said about them over time. An entity can exist here before we know where it is: a photograph of an unidentified building is enough to create one, and the location, the dates, and the name accumulate as evidence turns up. Sources are allowed to disagree, and the disagreement stays visible rather than being settled before it's stored. We use data and identifiers from projects like OpenStreetMap, Wikidata, and OpenHistoricalMap, and we intend to contribute data back as we build confidence in our results. See [Related work and where we fit](/related-work) for details on each project and how we integrate.

# Contributing

## Can historians, archivists, and researchers get involved?

Yes, and we'd like to hear from you. Chronoscope is meant to complement existing research, not replace it. We're especially interested in whole collections: photo archives, institutional datasets, anything that would otherwise take years of manual annotation to make usable. The ingestion pipeline is built to take them in as they are and keep their metadata and citations intact. If you have a collection you think belongs here, [reach out on GitHub](https://github.com/copumpkin/chronoscope/issues).

## Is it open source?

Yes! Chronoscope is open source under the MIT license.

## Who runs Chronoscope, and how is it funded?

Chronoscope is a project of the Chronoscope Foundation, formed to keep the project independent and accessible. {{foundation_status}} The code is MIT-licensed and the knowledge graph is released under Creative Commons Attribution 4.0, so the work stays usable no matter what happens to any one organization. The Foundation will be funded by donations and grants.

## How can I contribute?

We're working on the first full release of the site, which will let you contribute to the knowledge graph directly: linking photos to locations, adding historical context, and settling conflicts in the data. Until then, code contributions are welcome. The [GitHub repository](https://github.com/copumpkin/chronoscope) has the codebase, the ingestion pipelines, and the discussion in Issues.
