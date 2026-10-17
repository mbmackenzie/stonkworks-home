"""Command validation shared by HTTP and MQTT."""

from typing import Literal

from pydantic import BaseModel, ConfigDict, StrictBool, StrictInt, model_validator


class CommandFields(BaseModel):
    model_config = ConfigDict(extra="forbid", str_strip_whitespace=True)
    command: Literal["play", "pause", "play_pause", "next", "previous", "volume", "volume_relative", "mute"]
    value: StrictInt | StrictBool | None = None

    @model_validator(mode="after")
    def valid_value(self):
        if self.command in {"volume", "volume_relative"}:
            lower = 0 if self.command == "volume" else -100
            if type(self.value) is not int or not lower <= self.value <= 100:
                raise ValueError(f"{self.command} requires an integer from {lower} to 100")
        elif self.command == "mute":
            if type(self.value) is not bool:
                raise ValueError("mute requires a boolean value")
        elif "value" in self.model_fields_set:
            raise ValueError("Playback commands do not accept a value")
        return self
